# GDS memory work handoff

Prepared 2026-09-05. Companion to the [NBReq memory plan](nbreq_memory_plan.md).
This is a handoff document for a later session; no separate session has been started.

## Objective and entry point

Reduce GDS's network/RPC peak and retained memory. The owner clarified after M3 that the comms
server normally uses about 50–70 MB, but a large installation using 512 MB remains manageable.
Roughly 1 MiB is a usual message target, not a hard compatibility limit; retain the existing
24 MiB per-body ceilings. Target Windows and Linux, roughly 16–32 communicating connections
in the typical workload previously discussed, while allowing installation-specific scale.
Constrained 128/256 MB devices remain a separate profile requiring appropriate admission and
headroom. Establish actual incoming/outgoing concurrency, engine count, queue occupancy, and
encoded/decoded sizes before choosing limits.

The owner identifies GDS's control of simultaneous activity as the primary flexible control.
M4.1 workload admission is now implemented and verified; automatic
largest-connection eviction is not implemented or accepted. Distinguish HTTP socket count,
accepted/queued requests, and decoded work retained after HTTP completion. The current GDS
adapter exposes 32 sockets, 8 per origin, and 64 accepted/inflight requests per Engine; the last
replaces the inherited 1,024. The aggregate buffered-body cap is available but disabled unless
configured. Shared WebRPC controls sit ahead of the Engine; see the M4.1 return checkpoint.

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

## M2.5 GDS conversion return checkpoint — 2026-09-08

M2.5 is complete; see the [conversion report](nbreq_m25_gds_conversion.md). The owner
explicitly chose main-checkout nbreq 0.2 with local builds, superseding the earlier
isolated-checkout recommendation. GDS's manifest/lock now use 0.2. Build/test through
the existing local overrides until publication and a registry lock refresh.

Typed synchronous/waiter and legacy text conversion transfer unique response storage
into String, with an explicit copy when shared. Status, byte-limit and UTF-8 semantics
are preserved; JSON parsing continues to borrow. Early network enforcement remains M3.
Core Foundation dependency pins initially blocked resolution; the nbreq Darwin helper
now allows compatible patches, verified with 0.10.0 on both Mac toolchains.

Three allocation reds resolved. X86 GDS passes 14 HTTP/74 WebRPC tests, plus 2/73 in
ureq-only mode; both DLL configurations build without installation. Linux's exact
adapter harness passes 14/2 tests. Both Macs pass 393 library and 5 helper tests each
on stable and MSRV. Installed DLL SHA256 is unchanged; final cache is the native build.
Private GDS evidence is in gds/doc/evidence/nbreq-m25-20260908.tar.gz, alongside its
report and artifact identities. Unrelated WAL/optimiser edits remain outside this work.

M2 is accepted for its scope. No whole-GDS RAM or actual-device acceptance measurement
was made. Strings and decoded frames now owned by GDS still need admission budgeting;
broad queues/encoding remain here, and MQ-03 precedes M3 accounting.

## M3 controls return checkpoint — 2026-09-08

MQ-03 Option C is accepted. NBReq now has optional per-operation request/response ceilings and
an opt-in aggregate retained-buffer budget with a distinct `BufferedBodyBytes` limit reason.
The ledger covers buffered uploads, retained/shared replies, receive windows and extracted TLS
plaintext, including old/new allocation overlap; explicit unique transfer into GDS ends the
charge without freeing the resulting String. Exhaustion fails promptly without automatic replay.
Streaming queues and TLS session/record output overhead retain their separate budgets/headroom.

Light GDS integration is implemented in `dphttpclient.rs`. Existing request-specific response
ceilings reach nbreq before receipt. Startup environment knobs are
`GDS_NBREQ_MAX_REQUEST_BODY_BYTES`, `GDS_NBREQ_MAX_RESPONSE_BODY_BYTES` and
`GDS_NBREQ_MAX_BUFFERED_BODY_BYTES`; defaults remain 24 MiB per body with no aggregate cap.
Values are strict decimal byte counts; invalid values fail initialization. GDS's private
`gds/doc/nbreq_memory_controls.md` explains use and ownership. The existing retry classifier
continues to exclude Limit failures. No Delphi settings UI or app deployment was added.

The user says GDS generally limits responses to roughly 1 MiB because large Delphi allocations
and fragmentation are troublesome. Treat this as tuning guidance, not a replacement for valid
exceptional 24 MiB ceilings. Final values and large-operation policy remain M4/MQ-04. Numerical
acceptance must include GDS-owned Strings, encoding, queues and Delphi data after ownership
transfer. M3 is accepted: final E passes all seven nbreq full gates, x86 companions, 120 paired
memory cases and 36 longer timing cases. GDS native HTTP 16/WebRPC 74 and ureq-only 2/73 tests
pass; both DLL configurations build with SkipCopy and the installed DLL is unchanged. The
[M3 report](nbreq_m3_memory_controls.md) and E-13 artifact manifest retain measurements and limits.
Private GDS verification is in `gds/doc/nbreq_m3_verification.md` and its 45-file evidence archive.
All jobs finished. MQ-04 large-operation policy and MQ-05 whole-GDS/device acceptance remain open.

## M4.1 workload admission return checkpoint — 2026-09-08

The owner authorized this bounded GDS slice in the current session. Implementation B tracks
queued capacity through fetch/handoff, pauses heavy peers and shared intake, and reserves
outbound pipeline space before Rust formatting/encryption. It limits large deliveries and
protects small-work byte/worker headroom. A shared poll gate orders eligible waiting origins;
incoming work also slows when outgoing replies back up. Existing wire/body compatibility,
retry classification and shutdown behavior remain. No eviction, automatic response replay,
Delphi UI change or installed DLL replacement occurred.

Defaults: inbound pause/resume 32/16 MiB shared and 8/4 MiB per peer; outbound reservation
128 MiB with 8 MiB protected for frames up to 1 MiB plaintext; three large deliveries; 16 active
polls and four per origin. Reservations are not preallocated RAM. Native HTTP sockets/inflight
defaults are now 32/8/64 with startup checks that leave room beyond polls and large deliveries.
All are startup environment controls described in private `gds/doc/nbreq_m4_admission.md` and
`gds/doc/nbreq_memory_controls.md`. The nbreq body-capacity ledger stays optional and separate.

Seven runtime reds now pass. Windows x86 native HTTP/WebRPC 17/90 and ureq-only 2/89 tests pass;
both DLL configurations build with local nbreq and SkipCopy. Nine portable admission tests pass
on Windows x86 and Linux stable/MSRV. Component benchmarks measure bookkeeping only; full GDS
Linux integration and actual-device RAM/pickup-latency acceptance are not established by them.
Source/logs and corrected measurements are recorded by E-14 in the main tracker.

Next: run a representative GDS workload including idle/slow peers, same-origin bursts and mixed
small/large responses. Measure queue peaks, inbound pickup latency, outbound refusal and memory
retained after transfer to Delphi. Four same-origin long polls may increase pickup latency; tune
poll/socket capacity together. Soft inbound watermarks allow already active replies and decoding
to overshoot. The 128 MiB outbound allowance is not a total process cap or a 128 MiB device profile.
Choose an installation-specific nbreq aggregate cap only with headroom and valid-operation policy.

## Initial source facts (before M2.5/M3; see return checkpoints above)

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

M4.1 completed the explicitly authorized queue admission/poll scheduling slice above. This
handoff owns further RPC scheduling, batching,
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
