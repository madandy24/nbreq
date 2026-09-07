# E-09: M2 response ownership and completion delivery

Date: 2026-09-07. **M2.1/M2.2 verified.** Companion to the
[memory tracker](nbreq_memory_plan.md) and [GDS handoff](gds_memory_handoff.md).
MQ-02 Option C is accepted as MD-13. M2 remains in progress; M2.3 is next.

## Implemented scope and ownership contract

Existing `response.body()` borrowing and `.body().to_vec()` copying remain supported.
`Response::clone()` now shares immutable body storage. `Response::into_body()` consumes a
response and returns a `ResponseBody`; `ResponseBody::try_into_vec()` transfers the original
allocation when uniquely owned, preserving pointer, length and capacity. If another response
or body still shares it, extraction returns the owner unchanged in `Err`. The caller can drop
other owners and retry, or explicitly copy. There is no hidden copying fallback.

The implementation uses `Arc<Vec<u8>>`, avoiding a payload copy when adopting the existing Vec.
Response headers still clone normally. Equality compares contents. Empty and allocated-empty
buffers are supported. A body can outlive Engine shutdown without retaining workers or sockets.
The [getting-started guide](../docs/getting-started.md) shows copying and consuming conversions.
This removes copies in buffered response handling; it does not claim zero-copy network or TLS I/O.

Wait, timed wait, manual polling and callback delivery now move the completed result out of
request state. A separate permanent terminal marker prevents a second completion after the
payload has been consumed. Request/cancellation handles no longer retain a hidden body alias,
including while a callback is running. Admission, callback placement, once-only delivery and
joined shutdown retain their existing contracts.

**Aggregate budgeting is not implemented in this chunk.** MD-13 specifies future M3 behavior:
charge actual capacity through accepted buffered requests, staging/copies and unread, queued,
returned or shared NBReq bodies. Shared backing counts once; independent copies count separately.
Terminal state or cancellation alone cannot refund live storage. Last-owner release or explicit
unique transfer to an application-owned Vec ends the corresponding NBReq charge. Transfer does
not free RAM: GDS must budget its resulting Vec/String and decoded forms. M3 must attach a small
independent ledger without extending Engine worker/socket lifetimes. Application-provided storage
passed to public `Response::new` does not acquire a native Engine charge merely by being wrapped.
MQ-03 must settle reservation and exhaustion policy before that implementation.

## TDD and verification

The initial M1 implementation failed seven focused tests: shared clone storage, original allocation
delivery through wait/timed wait/manual/callback paths, and removal of retained canonical completion
storage. Introducing shared bodies and the new API produced 11 passes and two intended failures:
retained request state and unique extraction from callbacks still exposed hidden ownership.
Moving completion delivery resolved both. Final result: **13 ownership companions pass**.

Companions cover exact allocation identity/capacity; empty storage; shared extraction failure and
retry; independent mutable copies; content equality; cross-thread ownership; post-shutdown bodies;
callback activation before/after completion; queued permit lifetime; permanent terminal state;
late completion rejection; and once-only metrics/admission release. The same 13 tests execute
successfully on Windows x86. No unsafe code or new dependency was introduced.

| Full verifier | Final result | Duration |
| --- | --- | --- |
| Windows x64, Rust 1.97.1 | 24/24 steps pass | 279.158 s |
| Linux x64, stable 1.98.0 | 24/24 steps pass | 271.915 s |
| Linux x64, MSRV 1.85.0 | 24/24 steps pass | 324.007 s |
| Intel Mac, stable | 24/24 steps pass | 129.039 s |
| Intel Mac, MSRV 1.85.0 | 24/24 steps pass | 150.500 s |
| ARM Mac, stable | 24/24 steps pass | 66.042 s |
| ARM Mac, MSRV 1.85.0, unchanged repeat | 24/24 steps pass | 36.020 s |

These include the normal warning-denied, unit, integration, documentation and packaging gates.
Darwin support uses the established local override; registry-only publication remains the
previously documented `nbreq-darwin` release prerequisite. Nothing was published.

### Intermittent ARM test retained for follow-up

The first correctly rebuilt ARM MSRV run had 375 library tests pass and one existing streaming
test fail: `backend::native_http::tests::cancel_and_shutdown_wake_blocked_upload_producers`, at
`src/backend/native_http/tests.rs:4055`, expecting
`matches!(reader.try_head(), Err(StreamError::Cancelled))`. All new ownership tests passed.
The assertion does not print the actual terminal result, so the cause is **unproven**.

Five isolated repetitions passed with unchanged source, followed by a passing full unchanged
MSRV verifier. The first failure and all rechecks remain in the evidence as
`arm-evidence/verify-final-1.85.0-first-failure.log` and `arm-stream-recheck-1.log` through `-5.log`.
No assertion was weakened and no test was skipped. If this recurs, capture the actual outcome and
inspect fixture synchronization before deciding whether the fault is in production or the test.

## Paired Windows and Linux measurements

Compared frozen M1 C binaries with final M2 binaries using the unchanged M1 workload controller.
Each host ran 60 valid cases: three repetitions of five workloads, plain/allocation-meter modes,
and before/after versions. The order alternates across repetitions. There are **120 final valid
cases** in total. Ordinary cases use 32 connections with 1 KiB or 50 KiB bodies in each direction
over HTTP and HTTPS. The extended HTTPS workload also exercises 4 MiB upload/reply, streaming,
cancellation, retained results and recovery. Byte counts, outcomes, metrics, concurrency and
appropriate connection reuse checks pass; measured child processes are joined.

Plain binaries supply timing; the separate meter counts Rust allocations. Values below are
medians across three repetitions. Requested bytes are cumulative allocation/reallocation work
per completed request, not live memory or a direct measure of bytes copied.

| Host / ordinary workload | Requested KiB/request, before → after | Allocation + reallocation calls/request | Requests/s, before → after |
| --- | --- | --- | --- |
| Windows HTTP 1 KiB | 11.44 → 10.25 | 46.58 → 41.64 | 33,270 → 31,671 |
| Windows HTTP 50 KiB | 381.08 → 330.84 | 56.86 → 51.79 | 5,104 → 5,056 |
| Windows HTTPS 1 KiB | 16.00 → 14.82 | 53.61 → 48.49 | 29,940 → 31,148 |
| Windows HTTPS 50 KiB | 688.41 → 638.10 | 83.51 → 78.55 | 4,585 → 4,561 |
| Linux HTTP 1 KiB | 11.40 → 10.23 | 45.63 → 40.63 | 12,631 → 15,994 |
| Linux HTTP 50 KiB | 382.14 → 332.50 | 55.45 → 50.28 | 1,309 → 1,304 |
| Linux HTTPS 1 KiB | 15.84 → 14.68 | 52.63 → 47.66 | 13,764 → 14,283 |
| Linux HTTPS 50 KiB | 689.12 → 639.13 | 82.27 → 77.27 | 1,135 → 1,112 |

Both hosts show roughly one response-body allocation's worth of requested bytes and five
allocation calls removed per request. Peak Rust heap is essentially unchanged:

| Workload / peak across repetitions | Windows MiB, before → after | Linux MiB, before → after |
| --- | --- | --- |
| HTTP 50 KiB | 4.639 → 4.773 | 5.011 → 5.010 |
| HTTPS 50 KiB | 5.242 → 5.367 | 5.648 → 5.648 |
| Extended HTTPS 4 MiB upload/reply | 14.403 → 14.407 | 14.474 → 14.472 |

Request-side copies still dominate large transfers, and moving the original response Vec also
preserves its spare capacity. At the large retained-result endpoint, heap was 5.885 → 5.887 MiB
on Windows and 5.956 → 5.955 MiB on Linux. Retained capacity therefore remains M2.4 work.

Throughput ranges overlap and these short observations are noisy. Linux 1 KiB HTTP ranged from
5,090–12,878 requests/s before and 6,893–19,715 after; its median increase is not a reliable speedup
claim. Windows extended steady throughput had a lower median (4,688 → 4,331 requests/s), with
overlapping ranges of 4,477–4,771 and 4,213–4,671. This chunk establishes reduced allocation work;
it does not establish a universal speedup, guaranteed absence of regression, a major peak-RAM
reduction, or fit of GDS/the whole system into 128/256 MB. Compare changes within each host rather
than attributing the Windows/Linux throughput gap to software.

## Frozen source, evidence and reproduction

The [source archive](evidence/nbreq-m2-response-20260907.tar.gz) contains 117 hashed files plus
its manifest. Seven source/test/guide files differ from M1 C: `src/body.rs`, `src/types.rs`,
`src/registry.rs`, `src/client.rs`, `src/lib.rs`, `src/response_body_tests.rs`, and
`docs/getting-started.md`. Existing dirty M0/M1 and user work is preserved. Production source
remained unchanged through final verification and measurement.

- Source archive SHA-256: `c93b515df6a1cd369f962d4f4876d2eb692c003ef0f16dfd1a0ff78811eebbbf`.
- Source manifest SHA-256: `3704fc13a50678386a8189e12344f27ae084c5cdb143c1ae821877115d256302`.
- [Raw evidence archive](evidence/nbreq-m2-response-evidence-20260907.tar.gz): 806 files,
  974,019 bytes; SHA-256 `c93a9ecb0711cc0969c8ed2246e6a8582420e397ab0f23b101b7ef4ed3d2c7ff`.
- [Artifact receipt](evidence/nbreq_m2_response_artifacts.json) records hashes and final counts.

Archive members were compared byte-for-byte with their inputs. Evidence includes red/intermediate/
green logs, all final verifiers, the initial ARM failure, raw cases, summaries, binary provenance,
source checksums, excluded setup runs, bridge receipts and reproduction scripts.

For a current checkout, run the focused companions and then the full verifier:

```sh
cargo test --offline --all-features --lib m2_ -- --test-threads=1
cargo run --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline
```

For relocated source sharing a build cache, follow the final evidence script `verify-final.sh`:
rebuild xtask in a fresh runner directory and remove only NBReq package artifacts from the known
test cache before reusing dependency artifacts. The cached xtask embeds `CARGO_MANIFEST_DIR`;
early runs invoked the old M1 root. Relocated archive timestamps also allowed an old library to
survive after unit tests rebuilt. **Those results are excluded.** Final logs prove both the current
root and new M2 tests. Linux observer packages were also explicitly rebuilt. Only
`linux-evidence/comparison-final` is accepted; the earlier Linux comparison is excluded. The source
archive's original `m2-remote.sh` is superseded by the evidence archive's `verify-final.sh`.

`compare.py` records before/after plain and meter binary paths and hashes. Its final Windows
output is `windows-comparison`; Linux output is `linux-evidence/comparison-final`. Before binaries
match the archived M1 C provenance. The paired summaries are `windows-comparison-summary.json`
and `comparison-final-summary.json`.

Initial bridge quoting attempts failed before lab creation; corrected upload checksums and
launch receipts are retained. One Linux background-launch SSH client held its command channel
open. Only that exactly identified local client was stopped; remote logs and job state were
inspected before further work. No remote test or service was blindly restarted.

All jobs and measured processes finished. Warm `nbreq-m2-response-20260907` labs remain on Linux
(`/home/ubuntu`), Intel Mac (`/Users/andrew`) and ARM Mac (`/Users/m1`). Local working evidence is
under `target/m2`, with the durable copies linked above. No GDS implementation/dependency pin,
host trust store, publication, or commit changed.

## Next work

M2.3 removes request serialization/transport/replay copies while preserving fallback, redirects,
exact upload bytes and connection reuse. M2.4 addresses excessive retained capacity with evidence.
M2.5 applies the consuming conversion in GDS after reconciling its selected NBReq version/build;
status, size and UTF-8 rules must be preserved. MQ-03 precedes M3 reservation/accounting. Broader
GDS queues, encoding and whole-device acceptance remain in the handoff.
