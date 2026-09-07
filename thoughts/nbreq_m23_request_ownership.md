# E-10: M2.3 request ownership and bounded transmission

Date: 2026-09-07. **M2.3 verified.** Final D passes seven full verifiers; measured C production is
byte-identical to D. The 120 paired cases and 20 focused timing cases pass. All jobs have finished.
Companion to the [memory tracker](nbreq_memory_plan.md) and
[previous response ownership report](nbreq_m2_response_ownership.md).

## Scope and implementation

Native HTTP now serializes only the request line and headers during preparation. A pending
DNS lookup, connection-admission waiter, connecting socket or TLS handshake keeps the original
request body without building another whole-body wire buffer. Public `Request` borrowing,
building and deep cloning remain unchanged.

Cleartext HTTP borrows from the original body into a bounded send queue. The body window is
64 KiB plus the serialized head; smaller bodies use their actual length. Typical 1–50 KiB calls
still fit with their headers in one socket batch, including on reused connections. This is an
internal transmission window, not a new payload-size ceiling. Existing streamed-upload queue
policy is unchanged. TLS encrypts borrowed body slices through its existing bounded output path,
returning an exact consumed-byte count so the HTTP owner can advance its cursor. Borrowed
header/body slices share TLS records through vectored writes, avoiding an extra record merely
because their storage is separate. Handshake
worker ownership still excludes the body.

Request output is complete only when buffered body bytes, streamed upload state, TLS output and
the socket queue have drained. Cancellation discards unsent state; connection fallback keeps the
same body and deadlines and still ends once TCP connects. A failed reused socket is not silently
replayed after transmission begins.

Redirect policy is planned at the response head and applied by consuming the original request
after that response completes. Body-preserving redirects move the original Vec; 303 drops the
body according to existing method/framing rules. Deferring the move is necessary when a server
returns an early redirect while still receiving the upload. Credential stripping, downgrade
protection, hop limits and total deadlines retain their existing rules.

No aggregate budget, per-request API, GDS change, support-crate publication or commit is included.
M2.4 retained capacity, M2.5 GDS conversion and MQ-03/M3 accounting remain separate work.

## TDD and current proof

Six initial tests failed against E-09 at the intended checks: preparation, connection waiters,
cleartext connect state, TLS handshake state, the body-sized cleartext send queue and redirect
body copying. The 2 MiB fixture exposed a 2,097,224-byte duplicated wire allocation/queue limit.
Their original source and red output are retained under `target/m23`.

All six are now green, with three companions: partial-send cancellation; byte-exact 2 MiB
uploads through early 307/308 redirects and reuse with both buffered/streaming response readers
over HTTP/HTTPS; and borrowed TLS progress across zero/tiny output allowances, header boundaries,
empty bodies and reuse. Snapshot A passed nine focused tests, Windows x86 and all four stable
full verifiers (Windows 157.698 s; Linux 217.678 s; Intel Mac 103.458 s; ARM Mac 53.101 s).

All three A MSRV verifiers stopped at step 12 on a new fixture's `comparison_chain` lint. The
fixture now uses a match without changing its assertions. A's 60 Windows measurement cases passed,
showing lower allocation bytes/large-transfer peak but roughly two additional allocation calls
per HTTPS request. Inspection of pinned rustls and a new failing test proved that separately
writing the head/body emitted two TLS records for a small request instead of one. Snapshot B
uses borrowed vectored slices; **all ten focused tests and the lint preflight pass**. The TLS
record regression's original failure and source are retained alongside the six initial reds.

B passed Windows stable/x86, Linux and Intel stable/MSRV, and 120 Windows/Linux paired cases.
ARM B repeated the pre-existing `cancel_and_shutdown_wake_blocked_upload_producers` failure
previously seen in E-09. Its assertion did not print the actual terminal, so that original outcome
is unknown. Inspection exposed a concrete shutdown race: `begin_shutdown` published the owner
stop flag before committing all stream cancellations; backend teardown could drop a response
producer first and publish an Internal error. A deterministic regression holds the first stream's
permit-release mutex after its terminal commit, then simulates worker teardown if stop is already
visible. It failed with one Cancelled result and one Internal producer-ended result. No timing-based
retry is needed to prove this fault. The first fixture setup attempt used a spawned-only helper
with manual mode; that setup failure is retained separately and is not the red proof.

C publishes the stop flag only after terminal commits and shutdown bookkeeping. Admission still
closes immediately through the lifecycle state. The deterministic test, all 24 shutdown-focused
tests and ten request-memory tests pass; the integration assertion now prints the actual outcome.
C passed seven full verifiers, 120 paired measurement cases, and 30 ARM repetitions of the original
integration test. A/B results remain preliminary and do not prove the final C source.

C ARM has now passed both full suites and all 30 shutdown repetitions. The first C Windows
full verifier hit a separate existing DNS fixture assertion: Io instead of Truncated. Twenty
isolated rechecks and an unchanged full-verifier rerun passed. Inspection found that
`run_tcp_aware_dns` accepts from a nonblocking listener but does not normalize the accepted socket,
unlike already-corrected Windows HTTP fixtures. A direct Windows socket-mode probe using the
same accept/read-timeout setup returned WouldBlock immediately without normalization and
TimedOut after 256 ms with blocking mode restored. The fixture currently swallows the premature
read error and closes the socket, which can surface as Io to the resolver. The original failure
did not print its underlying socket error, so its precise execution is not retrospectively known.
D restores blocking mode in the fixture. Its Windows full verifier, x86 checks and 20 DNS
repetitions pass; both Mac and Linux stable/MSRV suites pass.
`production-identity.json` verifies C→D changes only `src/dns_wiring_tests.rs`, a module
compiled solely under `cfg(all(test, feature = "native"))`. All production and observer build inputs
are byte-identical, so C measurements apply to D production. No package/observer bytes were changed
during C measurements.
Do not treat planned measurements or dispatch receipts as completed validation. Any changed
source requires a new explicitly identified snapshot and renewed applicable checks.

## Measurements and assessment

The accepted comparison is E-09 response ownership → C request ownership/shutdown ordering.
Each host has 60 cases: HTTP/HTTPS at 1/50 KiB and an extended HTTPS workload, 32 connections,
three repetitions, before/after and plain/allocation-meter binaries. All 120 cases passed exact
byte/outcome, reuse, admission and joined-cleanup checks. Measurements use separate fixture/client
processes; throughput comes from plain binaries and logical heap/allocation counters from meter
binaries. Observer and baseline identities are recorded in the evidence.

Median cumulative allocation requested per completed ordinary request, KiB:

| Workload | Windows before → after | Linux before → after |
| --- | ---: | ---: |
| HTTP, 1 KiB | 10.37 → 8.25 | 10.23 → 8.16 |
| HTTP, 50 KiB | 330.83 → 230.96 | 332.36 → 232.35 |
| HTTPS, 1 KiB | 14.77 → 12.81 | 14.68 → 12.71 |
| HTTPS, 50 KiB | 638.05 → 538.11 | 639.29 → 539.28 |

That is roughly 100 KiB less allocation traffic per 50 KiB request, on top of the earlier response
delivery saving. These cumulative bytes are not resident RAM. HTTPS uses roughly one additional
allocation call per request with borrowed vectored input despite allocating fewer total bytes.
The small-request regression proves this does not add a TLS application record.

| Memory observation, MiB | Windows before → after | Linux before → after |
| --- | ---: | ---: |
| Peak live Rust heap during the 4 MiB transfer phase | 14.407 → 10.478 | 14.475 → 11.976 |
| Peak live Rust heap across the entire extended workload | 14.407 → 11.745 | 14.475 → 11.976 |
| Live Rust heap while holding the completed large response, median | 5.887 → 5.870 | 5.957 → 5.950 |
| Ordinary 32-connection 50 KiB HTTPS peak live heap | 5.274 → 5.262 | 5.643 → 5.642 |
| Extended workload process peak (Windows private commit / Linux RSS) | 20.570 → 19.898 | 28.676 → 24.902 |

The large-transfer live-heap peak falls about 27% on Windows and 17% on Linux. Ordinary small-call
peaks and the heap left after the large response completes barely change. The largest overall
Windows heap phase moves elsewhere in the extended workload, so its overall peak is higher than
the new large-transfer peak. Windows private commit and Linux RSS measure different things and
must not be compared as equivalent counters. None of these results establishes a whole-GDS or
128/256 MB device budget.

Plain steady-throughput medians, requests/s, from the three paired repetitions:

| Workload | Windows before → after | Linux before → after |
| --- | ---: | ---: |
| HTTP, 1 KiB | 20,761 → 21,865 | 15,724 → 16,970 |
| HTTP, 50 KiB | 4,443 → 4,859 | 1,449 → 1,417 |
| HTTPS, 1 KiB | 27,447 → 26,676 | 15,755 → 10,839 |
| HTTPS, 50 KiB | 4,095 → 4,051 | 1,098 → 1,136 |
| Extended HTTPS, 50 KiB steady phase | 3,580 → 2,706 | 1,063 → 1,201 |

Timing dispersion is material. The apparently large Windows extended slowdown did not recur in
five extra alternating pairs using the same frozen plain binaries: median 4,736 → 4,542 requests/s
(about −4.1%), with ranges 4,568–4,788 and 4,327–4,908. Median p95 latency was 5.949 → 6.550 ms.
Those ten cases passed all outcome/cleanup checks. Five equivalent Linux small-HTTPS pairs, run
after verification finished, also passed: median 12,280 → 13,342 requests/s, ranges 11,293–15,447
and 3,920–14,776; median p95 3.606 → 3.465 ms. The earlier large median slowdown did not reproduce,
but the after runs included a substantial latency/throughput outlier (p95 21.138 ms). All observations
remain in the archive. The evidence does not support a universal speedup or a claim of zero
throughput/latency regression. The measured memory improvement and preserved behavior justify
closing this copy-removal item; M4 must assess actual GDS/device performance and headroom.

M2.4 should address oversized retained capacity next, while preserving useful small-buffer and
connection reuse. M3 still needs explicit budgets/admission for concurrent large operations.
M2.5's GDS conversion remains unimplemented; no GDS source or version pin changed here.

## Frozen source and validation

Final test snapshot: `nbreq-m23-request-20260907-d`; measured production snapshot: `-c`.

- D source archive SHA-256: `4aebf84a8ff15c07c60233e3a48658432c5bdcd3d70d725f7e59c9ce2e7bea98`.
- D manifest SHA-256: `1c233f6a2627a1aea6383e6365d1a779ed5a6d2016eb31ba6321d5167e0afa62`.
- C source archive SHA-256: `fee874fba626645b1f05772597b5c340bb16290496352f3a3d5c66263086e806`.
- C manifest SHA-256: `003e80423d2eab3c70965384ae3615bd41c51f35f80922a9772b2ae22a418196`.
- B source archive SHA-256: `21d81385b4a8dc4d0cd8ff357bd63d2c832b396b8689805f619dddf169e2ad04`.
- B manifest SHA-256: `3a3c4ce23f6d05c94b5028cbbc75778ae44eccfaf2d1bcf2e117aa0b1131bf81`.
- Earlier A source SHA-256: `3f47a998d6252308215e52938e31fc7b426bd8a4f5839f903b1fd4b7b7488ecb`;
  manifest `bcd5a4f02a5c0156bda7c3224be376a69b0efed3dc530f2bc4e6fcf82ce95d81`.
- 119 hashed files plus the manifest; eight production/test files differ from E-09.
- Durable source/evidence: [artifact identities](evidence/nbreq_m23_request_artifacts.json),
  [final D source](evidence/nbreq-m23-request-20260907-d.tar.gz),
  [measured C source](evidence/nbreq-m23-request-20260907-c.tar.gz),
  [evidence archive](evidence/nbreq-m23-request-evidence-20260907.tar.gz).
  The artifact record also identifies retained A/B sources. The evidence archive contains original
  reds, setup failures, superseded attempts, all raw cases, source/binary identities, bridge receipts,
  verifier logs and per-file checksums; it was extracted in memory and checked byte-for-byte.
  Ignored `target/m23` working files are not the only record.

Changed files: `src/backend/native_http.rs`, `src/backend/native_http/http1.rs`,
`src/backend/native_http/tests.rs`, `src/backend/native_http/tests/request_ownership.rs`,
`src/backend/native_tls.rs`, `src/dns_wiring_tests.rs`, `src/registry.rs`, `src/types.rs`.

Windows C pipeline: `target/m23/c/windows.ps1`, logs and `windows.exit` in that directory. It ran
the full verifier, x86 memory/shutdown companions, release plain/meter builds, then 60 paired comparison cases.
The before binaries are frozen E-09 `target/m2/bin/plain.exe` and `meter.exe` with checked hashes.

Remote C labs ran the uploaded `nbreq/m23-remote.sh`; logs and `run.exit` are in each lab:

- Linux: `/home/ubuntu/nbreq-m23-request-20260907-c`; stable/MSRV then 60 paired cases against
  `/home/ubuntu/nbreq-m2-response-20260907/bin-final`.
- Intel Mac: `/Users/andrew/nbreq-m23-request-20260907-c`; stable/MSRV.
- ARM Mac: `/Users/m1/nbreq-m23-request-20260907-c`; stable/MSRV and shutdown repetitions.

All A/B/C/D jobs and measured processes finished. D labs use the same paths ending `-d`; receipts
are under `target/m23/d`.
D passes all seven full verifiers after the test-only fixture correction:

| Host | Stable full verifier, 24/24 | Rust 1.85 full verifier, 24/24 |
| --- | ---: | ---: |
| Windows x64 | 88.882 s | Not run on this host |
| Linux x64 | 208.940 s | 255.130 s |
| Intel Mac | 102.849 s | 106.163 s |
| ARM Mac | 53.277 s | 55.130 s |

Windows x86 additionally passes ten memory tests and 24 shutdown-focused tests; D Windows passes
20 DNS integration repetitions. C ARM passes 30 original shutdown integration repetitions.

C/D labs preserve the earlier logs and failure evidence under `previous-attempt`.
Each runner was rebuilt in a fresh directory
to avoid an embedded old checkout root. Only NBReq package artifacts are cleared in the known
`nbreq-m1-20260905-b/target` test cache; dependency caches and frozen baseline binaries remain.
Final logs prove the current root and new tests. Evidence collection validates source and binary
identities, outcome/byte checks, and joined observer cleanup before accepting cases.

## Reproduction and next gate

```sh
cargo test --offline --all-features --lib m23_ -- --test-threads=1
cargo test --offline --all-features --lib shutdown -- --test-threads=1
cargo run --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline
```

Next is M2.4 retained capacity, followed by the light M2.5 GDS conversion. Keep cumulative
allocation bytes distinct from live RAM and plain timings distinct from meter runs. MQ-03 still
precedes aggregate budgeting; support-crate publication and actual-device acceptance remain
separate gates. Nothing was published or committed in this work.
