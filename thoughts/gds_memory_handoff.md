# GDS memory work handoff

Prepared 2026-09-05. Companion to the [NBReq memory plan](nbreq_memory_plan.md).
This is a handoff document for a later session; no separate session has been started.

## Objective and entry point

Reduce GDS's network/RPC peak and retained memory within its usual 50–100 MB application budget.
Target Windows and Linux, roughly 16–32 communicating connections, mostly 1–50 KiB messages,
and constrained 128/256 MB devices. Establish actual incoming/outgoing concurrency, engine count,
queue occupancy, and encoded/decoded sizes before choosing limits.

Workspace: `C:\User\SecuritasNew`. Read its `AGENTS.md` and `PROJECT_OVERVIEW.md`, then the relevant
build/test workflow before changing or running GDS. Preserve the user's existing work. NBReq's
fourteen review regressions are now green and M1 is complete; see the checkpoint below.

## M1 return checkpoint — 2026-09-05

The [NBReq baseline](nbreq_m1_baseline.md) records 276 passing final observations on Windows/Linux
and additional x86/Mac checks. At 32 concurrent 50 KiB HTTPS calls, isolated test-client peaks were
10.03 MiB Windows private commit and 11.61 MiB Linux RSS; measured Rust heap peaks were 5.03 and
5.65 MiB. These are different process counters, and neither measures incremental cost inside GDS.
A 4 MiB buffered upload/reply reached roughly 14–17 MiB live Rust heap across the measured HTTP/
HTTPS cases. Large frames and concurrent retained results still need deliberate admission.

NBReq now has Engine-specific additional DER roots that supplement platform trust, allowing
private-CA deployments without changing the host trust store. Windows/Linux/macOS behavior is
verified. The GDS version pin and adapter have **not** been changed; reconcile the deployed build
before applying this or any 0.2 API. No GDS implementation was part of M1.

## M2 response ownership return checkpoint — 2026-09-07

MQ-02 Option C is accepted (MD-13), and M2.1/M2.2 are implemented and verified; see the
[ownership report](nbreq_m2_response_ownership.md). Existing borrowed/copying consumers remain
supported. `Response::clone()` shares immutable body storage, `Response::into_body()` moves its
owner, and `ResponseBody::try_into_vec()` transfers the original allocation when uniquely owned.
Sharing returns the owner unchanged so the caller can release aliases or explicitly copy.
Internal HTTP completion delivery moves the payload without retaining a hidden body alias.

The ordinary GDS path should support no-copy extraction and UTF-8 String conversion. Check
status/size rules before conversion. Resulting Vec/String and decoded forms belong to GDS and
need application budgeting. No GDS code or dependency pin changed; reconcile the selected
version/build before integrating this 0.2 development API (M2.5).

The future M3 budget will retain charges through NBReq body owners, ending on last-owner drop
or explicit unique Vec transfer. **That budget is not implemented yet.** Early per-request
enforcement is also M3. Paired Windows/Linux measurements (120 valid cases) confirm about
50 KiB less cumulative allocation per 50 KiB response; the large 4 MiB upload/reply peak was still
about 14.4 MiB at that checkpoint. The subsequent M2.3 results are recorded below; retained
capacity was assigned to M2.4 (now completed below). Broader queues, encoding and whole-device acceptance remain with this
handoff. Valid ceilings are unchanged.

## M2 request ownership return checkpoint — 2026-09-07

M2.3 is verified across all four hosts; its [request ownership report](nbreq_m23_request_ownership.md)
records removal of whole-body
serialization/copies in pending DNS/connect/TLS work, bounded cleartext output, borrowed TLS input,
and consuming body-preserving redirects. The existing public request API and valid large-message
ceilings are unchanged. GDS gains this internal improvement when it integrates the selected NBReq
version; no request-side adapter rewrite is required. No GDS files or dependency pins changed here.

The 120 paired Windows/Linux cases show about 100 KiB less cumulative allocation per 50 KiB request,
on top of the earlier response-side saving. A 4 MiB transfer's peak live Rust heap falls from
14.4 to 10.5 MiB on Windows and 14.5 to 12.0 MiB on Linux. Ordinary small-call peaks change little,
and roughly 5.9 MiB remains while holding the completed large response. These measurements are
isolated client observations, not incremental GDS RAM. Throughput results are noisy; HTTPS adds
roughly one allocation call, and the report preserves the measured tradeoffs and focused rechecks.

At that checkpoint M2.4 retained capacity and M2.5's light GDS response conversion remained next. Aggregate budgets and
early request-specific enforcement are still M3. Broad GDS queue/encoding admission and acceptance
on actual 128/256 MB targets remain in this handoff. Check the live tracker for final verification
status and source identities before integrating.

## M2 retained-capacity return checkpoint — 2026-09-08

M2.4 is verified; see the [retained-capacity report](nbreq_m24_retained_capacity.md). Fully consumed
TLS streaming plaintext now releases its unused allocation. Completed reusable HTTP connections
trim send capacity above 128 KiB to 64 KiB; ordinary smaller buffers and connection/TLS reuse stay
available. This is internal retention policy, not a reduction in valid request/response ceilings.

The mixed streaming/large-transfer workload's live Rust heap after releasing its large response
falls from 1.88 to 0.94 MiB on Windows and 1.95 to 1.01 MiB on Linux. Ordinary small-call retention
and allocation churn are essentially unchanged. Seven full platform gates, x86 companions, 120
paired cases and 30 longer Windows timing cases pass. Timing is noisy, smaller latency costs are
not excluded, and repeated large uploads may regrow/retrim buffers. These isolated measurements
do not establish incremental GDS RAM or acceptance on a 128/256 MB device.

No GDS source, dependency pin or deployed build changed. M2.5 is next: reconcile the selected
0.1/0.2 version/build path, then use consuming response extraction where ownership permits,
preserving status/size/UTF-8 handling. M3's early limits and aggregate retained-body charges are
still unimplemented; broad GDS admission/encoding and actual-device acceptance remain here.

M2.5 inspection on 2026-09-08 confirms two copying sites in `dphttpclient.rs`: `execute_text`
and the shared synchronous/waiter `nbreq_response_to_dp` conversion. Check byte limits before
extraction, transfer unique storage into String, and explicitly copy if another response shares
the body. Preserve typed non-2xx responses and the text convenience path's status-first errors.
The `post_json` response parser already borrows bytes. Details and test gates are in the tracker.

Both GDS manifest and lockfile still select 0.1.0. Existing `--local-nbreq`/`-LocalNbreq` helpers
keep a private temporary lock and check the selected package, but deliberately require a matching
manifest version. Recommend a separate GDS development checkout with an explicit 0.2 requirement
and those local overrides; registry integration follows its release prerequisites. Update the
wrappers' hard-coded 0.1.0 source labels with the transition, and compile with `-SkipCopy` during
validation. This inspection made no GDS changes and did not run GDS tests or replace its DLL.

## Observed source facts

| Location under `C:\User\SecuritasNew` | Finding |
| --- | --- |
| `gds/rust/gds/src/dplib/dphttpclient.rs`: `NBREQ_ENGINE_MAX_BODY_BYTES`, `create_nbreq_http_client` | GDS selects 24 MiB request/response ceilings, leaving other NBReq defaults in place. The comment explicitly accommodates large WebRPC frames. |
| Same file: `execute_response`, `NbreqHttpWaiter`, `nbreq_response_to_dp`, `bytes_to_response_text` | Request-specific response limits are applied after buffered completion. Conversion calls `response.body().to_vec()` before checking the smaller limit. |
| Same file: `execute_text` | Text conversion also copies response bytes because the current adapter uses the borrowed body accessor. |
| `gds/rust/gds/src/dplib/dpwebrpc.rs`: constants near `MAX_OUTBOUND_DELIVERIES` | Per-instance limits include 1,024 inbound requests, 256 outbound deliveries, 256 requests per batch, 24 MiB poll bodies, 18 MiB decoded data, and 16 MiB plaintext. POST responses have a 64 KiB limit. |
| Same constants and assertions | The comment cites a measured 55,791-byte outbound plaintext maximum and deliberately generous compatibility headroom. Compile-time assertions require the engine ceiling to accommodate the allowed poll and encoded outbound frames. |
| `gds/rust/gds/src/dplib/dpsyscontext.rs`: HTTP service construction/access/shutdown | The system context already owns and reuses a shared HTTP service/engine. Retain that lifecycle rather than creating engines per operation. |

At 50 KiB per queued item, count limits alone permit about 50 MiB inbound and 12.5 MiB outbound
payload per full queue, before metadata or encoding overhead. These are capacity illustrations,
not measured queue occupancy. Large base64, encrypted, plaintext, string, and parsed forms may
overlap in memory; instrument their actual lifetimes.

Version caveat: this checkout's `gds/rust/gds/Cargo.toml` currently pins `nbreq = "=0.1.0"`, while
the NBReq review covers the 0.2.0 development tree. Verify the selected branch, lockfile, package,
and running build before applying APIs or drawing runtime conclusions. Historical release notes
and isolated worktrees may describe different versions. No dependency pin was changed in this pass.

## Boundary with the NBReq session

The NBReq session may make light adapter/configuration changes: tune existing engine admission,
use a consuming body API when available, and propagate the existing per-request limits for early
enforcement. Those changes must preserve valid requests, wire formats, and shutdown behavior.

This handoff owns broader changes: RPC workload scheduling, byte-based queue admission, batching,
poll/backoff policy, encoded/decoded/frame ceilings, and incremental large-frame processing.
Do not silently lower protocol limits to typical observed sizes. Determine exceptional operations
and compatibility needs, then choose explicit small-device policy, splitting/streaming, or bounded
rejection with a recoverable caller-visible result.

## Proposed work

1. Measure queue bytes and high-water marks, message-size distributions, concurrent operations,
   outstanding deliveries, and memory held by each encoding/decoding stage. Include slow consumers,
   recovery/reconnect bursts, and more than one WebRPC instance/system context.
2. Bound work at admission with both item counts and byte budgets. Account for all instances sharing
   the application budget. Decide what happens on pressure: delay polling, split a batch, or return
   an explicit error. Preserve ordering and delivery semantics; avoid silent drops and retry storms.
3. Revisit the 24/18/16 MiB compatibility ceilings using requirements and measured outliers. Budget
   overlapping representations, not just the HTTP wire body. Design large-operation handling if
   those limits remain necessary on small devices.
4. Coordinate with NBReq's per-request early limits, consuming response API, and aggregate buffered
   budget. State what GDS must bound after a response becomes application-owned.

## Acceptance and return handoff

- Prove intended limits before large allocation: both declared-length and chunked responses,
  plus decoded/plaintext expansion. Include exact-boundary and oversized fixtures.
- Show bounded queue bytes across instances and correct recovery after pressure clears.
- Preserve long-poll cancellation, POST ordering/delivery, normal joined shutdown, and valid large
  operations under the chosen compatibility policy.
- Measure Windows/Linux process and allocation peaks, post-burst idle retention, latency, and CPU
  with typical messages and exceptional frames. Separate NBReq overhead from RPC/application data.
- Return chosen limits, compatibility decisions, exact source/build identity, commands/results,
  and any required NBReq API changes to the main memory plan. Keep broad GDS changes independently
  reviewable; do not mix them into the P1/P2 correctness fixes.
