# NBReq memory work tracker

Opened: 2026-09-05 · Updated: 2026-09-15 · M0–M3 accepted; M4.1 verified; R4/R5 technical/presentation checks, main housekeeping/push and updated hosted CI complete. Publication/registry gates are next; device profiles remain separate.

This is the working record for M0–M4. It adopts the work-package, contract, verification, and log
structure of the [post-release follow-up plan](project_nbreq_followup_plan.html). Keep this file
current as work proceeds. The [red-test record](nbreq_review_red_tests.md) preserves the original
failure evidence; the [GDS handoff](gds_memory_handoff.md) owns broader consumer changes.

## 1. Resume checkpoint — read this first

| Field | Current state |
| --- | --- |
| Current package | **M4.1 GDS workload admission verified — E-14.** M3 accepted — E-13; M2 accepted — E-09–E-12. |
| Next implementation step | Follow the [release checklist](nbreq_020_release_plan.md) for helper publication, registry-only proof and root publication. Local packages/Windows consumers pass at `432509b`; main `451a769` is pushed and all 10 hosted jobs pass after one unchanged Intel Mac rerun. [Housekeeping](nbreq_housekeeping.md) retains its unconfirmed fragmented-response timeout. [W-01](nbreq_wine_dns.md) has actual Wine 5 x86 and native Windows evidence plus the successful multi-day GDS pilot report; formal pilot provenance/shutdown details remain unrecorded. Resume GDS workload/headroom measurements when available; M4 device profiles remain separate. |
| Last proven test state | M4.1 final B: Windows x86 native HTTP 17/WebRPC 90 and ureq-only 2/89 pass; both DLL configurations build with SkipCopy. Nine portable policy tests pass on Windows x86 and Linux stable/MSRV. Seven runtime reds preserved. Corrected C timings leave B production unchanged. Release R1/R2 adds seven full nbreq verifiers and stable/MSRV packaged-consumer checks across Windows/Linux/both Macs. Before R1 all 119 ordinary M3 E inputs matched; R1 changes docs/examples/doc comments, with memory runtime implementation unchanged. Installed DLL unchanged; all jobs finished. |
| Source identity | R5 runtime checkpoint `bdf5db5` includes Wine discovery, the pre-R5 planned-body reservation fix and patched rustls minimum. Presentation commit `655a6e3` adds the final README/examples. Main's original state is saved in `cb18acc` on `codex/main-before-housekeeping-20260915`; its useful F5/report differences are adopted for integration. See [housekeeping](nbreq_housekeeping.md) for current identities. Historical M3 E evidence remains intact; GDS private source/logs remain in its repo. |
| Memory implementation | nbreq retains M3's policy with the pre-R5 planned-body reservation correction and its regression coverage. GDS M4.1 adds process-shared queued-byte watermarks, outbound reservations and small-work reserve, local large-delivery limits, eligible-origin poll admission and configurable 32/8/64 socket/inflight defaults. Incoming polls also slow under outgoing backlog. Retain 24 MiB body ceilings; nbreq aggregate cap stays opt-in. No automatic eviction. |
| Pending choices | MQ-01/MQ-02/MQ-03 accepted. MQ-04's initial admission policy is implemented under MD-20; exceptional frames retain compatibility within available allowance. Splitting/streaming for constrained profiles is deferred. MQ-05 remains open: typical 50–70 MB comms usage and manageable 512 MB large installs do not establish a 128/256 MB device profile. |
| Workspace caution | Housekeeping preserves the original main state and reconciles its F5 tooling/reports with R5. Keep the local snapshot branch and ignored build caches; older evidence archives retain their original source/dependency identities. |

The accepted statuses below retain historical evidence. On 2026-09-10 the user cautioned that
tests may not be authoritative pending investigation of an nbreq Rust panic under Wine (W-01).
Native-host passes do not establish Wine coverage. The later [W-01 investigation](nbreq_wine_dns.md)
provides actual Wine 5 x86 reproduction and a verified byte-parser fix. The 2026-09-14 owner report
adds several successful live-pilot days; it does not substitute for the controlled release soak.

The earlier M2 commit preparation removed one trailing blank line from the allocation-meter
Cargo manifest; its parsed TOML was unchanged. Memory runtime implementation remains M3 E;
the later release work adds public docs/examples, source doc comments, an independent consumer
harness and the scoped Windows DNS fixture correction. W-01 subsequently replaces Windows adapter
discovery in `149450d`, mirrored narrowly to main; memory algorithms/limits remain M3 E. Its source,
package identities and actual Wine/native results are recorded in the release checklist.

M4.1 changes only GDS runtime/test source: final B's WebRPC SHA256 is
`0f92b327998f6e2ebae1a7b4ba81b6475a04c46ba7e157df62c258ce8168d07f`; the artifact manifest
records the HTTP adapter and private admission module too. E-14 archives 191 private files under
GDS documentation, SHA256 `0c48b565568b7e5f5aa6733a0f40dd2eeb0d6eb516c814fe4ec3a6a3d6bda6a5`.
Measurement C corrects timing only; A/B are retained as superseded timing evidence. Source B
and all passing/failing test/build logs are preserved. Linux A/B/C jobs have finished in
`/home/ubuntu/gds-m4-admission-20260908-{a,b,c}`. No commit or deployment occurred in this step.

M3 is complete: five frozen source snapshots and 2,696 public evidence files are archived in
E-13; 45 private GDS files are archived in its repository. Read the report before inferring a
whole-process cap or selecting values. The longer 1 KiB timing runs do not reproduce a material
throughput penalty, but some Linux p95 cost remains unexcluded. No final GDS/device profile,
installed DLL replacement, or registry publication occurred. Final remote labs are
`/home/ubuntu/nbreq-m3-20260908-e`, `/Users/andrew/nbreq-m3-20260908-e`, and
`/Users/m1/nbreq-m3-20260908-e`; all jobs and measured child processes have finished.

M2.4 is complete: source B, all seven full gates, x86 companions and 150 measurement/timing
cases are archived in E-11. Working/remote `nbreq-m24-retention-20260908-b` labs retain binaries
and logs; A remains as failed-fixture evidence. M2.5 subsequently relaxes the Darwin dependency
manifest; M2.4 production/test code remains B. The longer Windows check reduces apparent large throughput regressions, but small latency costs remain
unexcluded. M2.5 is complete (E-12), as is M3 (E-13); actual-device/GDS memory acceptance remains open.

All M2.1/M2.2 jobs and measured processes have finished. Fresh `nbreq-m2-response-20260907` labs
retain source/binaries/evidence. Across checkout changes, rebuild xtask in a fresh runner folder
and clear NBReq package artifacts before reusing dependency caches; check the reported root and
new tests. E-09 records this procedure. Budgeting was not implemented at that checkpoint; M3
now supplies it. The later GDS adapter completion is recorded in E-12.
The ARM streaming assertion recurred during M2.3 B. E-10 records a deterministic shutdown-ordering
fault and C fix, seven C full verifiers and 30 passing ARM repetitions. D corrects a separate
Windows DNS test fixture and passes all seven final verifiers. Earlier failures remain visible;
their original uninstrumented socket/terminal outcomes cannot be reconstructed exactly.
All M2.3 source snapshots and evidence are archived; remote `nbreq-m23-request-20260907-c` and `-d`
labs retain measured binaries/source/logs. The 20 focused timing follow-ups did not reproduce the
large median slowdowns from the noisy main batch; they do not establish zero performance cost.

All M1 jobs and measured child processes have finished. Final `nbreq-m1-20260905-c` labs retain
source/binaries/results; C reuses the preceding `-b/target` build caches. Durable evidence is linked
from E-08, so ignored `target/m1` logs are not the only record. Windows x86 runs under WoW64.
Registry-only packaging
now has a confirmed release prerequisite: online Cargo on the bridge hosts cannot find
`nbreq-darwin` on crates.io. The owner confirmed that this support crate needs its normal
publication process, similar to winpoll. Local support overrides allow archive compilation and
unit tests. Nothing was published. The 128/256 MB whole-device target and GDS integration remain
unproven; E-08 measures the isolated NBReq test client on larger hosts.

Resume by reading this checkpoint, the selected work item, and its evidence/decision references.
Reconcile them with the current checkout before using these reproduction commands:

```sh
cargo run --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline
cargo test --offline --all-features --lib review_p -- --test-threads=1
cargo test --offline --all-features --test package_contents review_p -- --nocapture
```

Run the library and packaging commands separately; Cargo stops after a failing test binary.

## 2. Programme boundary and agreed principles

The target is Windows/Linux consumers with roughly 16–32 communicating connections, mostly
1–50 KiB messages, and intermittent bursts. GDS's total application budget is normally 50–100 MB;
128/256 MB devices are the constrained target. Distinguish NBReq outgoing connections from GDS
inbound clients, quiet/long-poll connections, and work waiting in application queues.

| In this programme | Separate or deferred |
| --- | --- |
| Existing P1/P2 fixes; NBReq allocation/retention work; per-request limits; buffered and streaming budgets; measured profiles. M4.1's bounded GDS admission slice is now explicitly authorized. | Further GDS batching, incremental encoding/decoding and protocol/frame-policy changes: use the GDS handoff. |
| Light GDS work through its shared engine: admission tuning, consuming response bytes, and propagating existing request-specific limits for early enforcement. | Protocol changes, silent reduction of valid large-message limits, custom allocators, broad pools, TLS replacement, and speculative stack tuning. |

Agreed principles:

1. Preserve Engine ownership, cancellation, joined shutdown, callback placement, passive manual
   APIs, exact delivery, keepalive, and existing supported-platform behavior. Windows/Linux are
   the memory measurement target; this does not remove existing platform obligations.
2. Bound resource acquisition and buffer growth before it happens. Free retained allocations
   before refunding their budget. Count limits and byte limits solve different problems.
3. Distinguish reservations, allocated capacity, live payload bytes, allocation churn, and process
   memory. TLS, platform verification, sockets, stacks, and caller-owned data need separate headroom.
4. Keep ordinary small calls convenient. Preserve useful small-buffer reuse and connection reuse.
   Choose larger-operation policy explicitly; candidate values below are experiments, not defaults.
5. Fix the current red tests first. Add focused companion tests with each fix; new memory APIs and
   performance experiments are not prerequisites for unrelated correctness work.

## 3. Work packages and dependencies

Status vocabulary: **Open** = not started; **In progress** = active work; **Verified** = implemented
with linked checks for a stated scope; **Accepted** = the full package gate is met; **Deferred** =
recorded outside the active sequence. Test code existing, or a document being approved, does not
make an implementation package accepted.

| ID | Status | Dependency | Deliverable and acceptance gate |
| --- | --- | --- | --- |
| M0 | Accepted — Windows x64/x86, Linux x64, both Macs; E-06/E-07 | Existing red evidence | M0.1–M0.9 fixed. All fourteen regressions and 23 companions pass in applicable suites; normal verifiers pass without skips. Remote stable/MSRV and Darwin helper gates pass. Registry-only packaging awaits support-crate publication before release. |
| M1 | Accepted — E-08 | M0 | Separate-process F5 observer and exact logical allocation meter; 120-case Windows/Linux baselines each, plus 12 cases each on Windows x86 and both Macs. Cold/steady/burst/recovery, extended workloads, fixture overhead, latency/CPU and limitations documented. |
| M2 | Accepted — E-09–E-12 | M1 | Ownership/copy and retention work verified with platform gates and paired measurements. Light GDS conversion is verified on x86, exact Linux adapter and Mac dependency compatibility. Whole-GDS/actual-device acceptance remains M4. |
| M3 | Accepted — E-13 | M1, M2, MD-16 | Early per-request limits and aggregate retained-body controls implemented, including light GDS startup knobs. Ownership/reservation/release/progress proved; seven full gates, x86 companions and paired Windows/Linux observations pass. Final settings remain M4. |
| M4 | In progress | M2, M3, applicable GDS decisions | M4.1 admission is verified. Select measured installation/device profiles; record workload limits, headroom, compatibility and pickup-latency/performance tradeoffs. Whole-GDS/actual-device acceptance remains open. |
| M4.1 | Verified — E-14 | M3, MD-20 | Shared GDS workload admission, queue/handoff accounting, small-work protection and polling limits. Seven runtime reds resolved; native/ureq x86 suites and builds plus portable Linux stable/MSRV policy checks pass. Component measurements do not establish end-to-end acceptance. |

Dependency sequence: **M0 → M1 → M2 → M3 → M4**. Scope M2/M3 into smaller stable sub-IDs when
implementation details are concrete; do not renumber completed items. Broad GDS work may proceed
in its own session but does not expand the M0 correctness pass.

### M4.1 implementation checkpoint — 2026-09-08

The owner authorizes practical workload admission to control ordinary retention and pathological
pressure while protecting small requests. First slice: expose existing nbreq connection/inflight
controls in GDS; track inbound queued bytes through poller handoff, pause/resume polling with
global and per-instance watermarks; reserve outbound pipeline bytes before formatting/encryption;
limit large deliveries so they cannot occupy all four existing POST workers, with a protected
small-work byte allowance. Shared production admission spans WebRPC instances. Preserve 24 MiB
body compatibility and protocol/retry/shutdown semantics. No automatic connection eviction.
Seven failing integration tests are fixed and the final B suites/builds pass. Shared inbound
32/16 MiB and peer 8/4 MiB watermarks pause/resume polling. The 128 MiB outbound reservation
allowance protects 8 MiB for small frames and pauses intake at 96 MiB until it drains to 64 MiB.
Three large deliveries cannot consume all four local POST workers. Poll admission is 16 total,
four per origin, with configurable native HTTP 32/8/64 socket/inflight defaults and startup
headroom checks. These allowances are not upfront allocations. Read the GDS private
`gds/doc/nbreq_m4_admission.md` for exact environment controls and overload semantics.
Final source is B; portable measurement C fixes a worker-timing flaw without changing B policy.
Private source/evidence lives in the GDS repository; E-14 records identities. Actual-device/process
acceptance remains separate. Soft watermarks do not bound already active replies or Delphi-owned
data, and fewer same-origin polls can increase pickup latency. No live DLL replacement occurred.

### M2 item register

| ID | Status | Deliverable / acceptance |
| --- | --- | --- |
| M2.1 | Verified — E-09 | Buffered wait/timed wait/manual/callback delivery moves the original payload and retains a permanent terminal marker. No hidden body alias; exact-once/admission/lifecycle behaviour preserved. Red/green and all platform gates pass. |
| M2.2 | Verified — E-09 | Shared immutable ResponseBody and explicit unique Vec extraction preserve copying usage, empty/spare capacity, shared failure/retry, cross-thread and post-shutdown ownership. Thirteen companions, seven full verifiers and 120 paired cases pass. Less allocation churn; peak memory essentially unchanged. No aggregate budget yet. |
| M2.3 | Verified — E-10 | Header-only preparation, bounded borrowed HTTP/TLS transmission, consuming redirect bodies; original request API/ceilings preserved. Six ownership reds, TLS-record red and deterministic shutdown-order red resolved. Ten memory tests, seven final full verifiers, x86, 120 paired cases and 20 timing cases pass. Large-transfer peaks fall; retained capacity remains M2.4. |
| M2.4 | Verified — E-11 | Release fully consumed TLS plaintext; trim reusable HTTP idle send capacity above 128 KiB to 64 KiB. Three intended reds resolved, six companions, seven full verifiers, x86, 120 paired cases and 30 timing follow-ups pass. Preserve partial data, small buffers, connections and valid ceilings. Post-workload retention falls; allocation churn is essentially unchanged. |
| M2.5 | Verified — E-12 | Unique response-to-String transfer with shared copying fallback; status/limit/UTF-8 contracts preserved. Three allocation reds resolved; x86 HTTP/WebRPC, native/ureq DLLs, Linux adapter and both Mac dependency gates pass. GDS manifest/lock moved to local 0.2; installed DLL unchanged. |

### M2.5 completed scope — 2026-09-08

Checkpoint 2f89bef records M0–M2.4; 2c382b7 records the initial GDS inspection.
The user then cleared GDS and chose main-checkout nbreq 0.2 with local builds (MD-15),
superseding the isolated-checkout recommendation. E-12 records final source and proof.

GDS consumes unique response storage into String in typed sync/waiter and legacy-text
paths. Unexpected sharing explicitly copies; typed non-2xx status/body, byte limits
before UTF-8, and legacy status-first/detailed errors are preserved. JSON parsing still
borrows. Three allocation reds resolved and five additional tests cover semantics and
real delivery. No request-body, transport-policy, lifecycle or 24 MiB ceiling change.

GDS manifest and lock now select local nbreq 0.2. Its local helpers retain private
resolution and selected-path checks, now also supporting ureq-only diagnostics.
Cargo exposed incompatible exact Core Foundation patches; nbreq-darwin now permits
compatible 0.10 patches. GDS selects 0.10.0; the normal nbreq lock remains 0.10.1.
Both Macs pass lower-patch library/helper tests on stable and Rust 1.85.

Windows x86 HTTP/WebRPC and both DLL builds pass with SkipCopy; the installed DLL
hash is unchanged, and the final build cache is native. Linux tests the exact adapter
source in a focused harness, not the full GDS DLL. E-12 preserves all evidence and
scope limits. All remote jobs have finished. Nothing was published or deployed.

M2 is accepted for its defined scope. MQ-03 precedes M3's early limits and aggregate
budget. Broad GDS queue/encoding policy and actual 128/256 MB device acceptance remain
in the handoff. Unique transfer removes a copy; it does not remove GDS-owned memory.

### M0 item register

All items below are implemented and verified on Windows x64/x86 (E-06), Linux x64, and physical
Intel/Apple Silicon Macs (E-07; stable 1.98 and MSRV 1.85). Target-specific test counts differ.
Test filters have the prefix `review_`; full names and original failure details remain in the
historical red-test record.

| ID | Finding / filter | Status | Fix constraints and companion coverage |
| --- | --- | --- | --- |
| M0.1 | P1 TLS stall: `p1_certificate_verification_` | Verified — E-05/E-06/E-07 | MD-06 implemented: two lazy workers, four queued steps, six total tickets, 16 KiB input windows, HTTP body separation, cancellation/deadlines, and joined shutdown. Original red plus 11 companions pass. |
| M0.2 | P2 HTTP address fallback: `p2_http_tries_the_next_dns_address_` | Verified — E-06/E-07 | Serial attempts retain one request/body allocation, connection reservation, and original deadlines. Cancellation/exhaustion complete once; successful TCP ends fallback to prevent replay. Three companions pass. |
| M0.3 | P2 DNS cache truncation: `p2_dns_cache_preserves_` | Verified — E-06/E-07 | Cache full wire-bounded answers; cap only delivered results. Both-family refresh/order companion passes; parser packet, cache entry, and TTL bounds remain. |
| M0.4 | P2 TCP send spin: `p2_tcp_blocking_send_waits_` | Verified — E-06/E-07 | Wait for enough space for the whole chunk. Two companions verify partial progress, sufficient-progress wakeup, cancellation, and the entire unaccepted suffix. |
| M0.5 | P2 TCP abort retention: `p2_tcp_*_frees_` (four tests) | Verified — E-06/E-07 | Drop payload and outer queue allocations before refund, reset offsets/counters, and keep repeated abort idempotent. Companion observes freed allocations inside the refund callback; graceful-FIN tests pass. |
| M0.6 | P2 repeated finish callback: `p2_tcp_finish_callback_registration_` | Verified — E-06/E-07 | Permanent registration state survives delivery; concurrent registration companion admits and delivers exactly one callback. Rejected callback captures are dropped outside the state lock. |
| M0.7 | P2 HTTP 205 framing: `p2_205_` (three tests) | Verified — E-06/E-07 | Exact/fragmented chunk and streaming completion pass. Two companions cover explicit zero length, close delimiting, HEAD/204/304 exceptions, malformed chunks, and premature EOF. |
| M0.8 | P2 DNS ID sequence: `p2_dns_wire_ids_` | Verified — E-06/E-07 | Fresh OS entropy per allocation and bounded collision search. Two companions cover entropy failure, wraparound, last-free-ID search, reuse, and exhaustion; existing HTTP-reserve tests pass. |
| M0.9 | P2 missing package fixtures: `p2_package_contains_` | Verified — E-06/E-07 | Seventeen byte-identical shared seeds moved outside the nested fuzz package. Archive inventory and extracted unit suites pass on Windows x64 and all three remote hosts with local support overrides; Windows x86 inventory also passes. Registry-only packaging awaits `nbreq-darwin` publication. |

The `*` in the M0.5 description is shorthand, not a Cargo wildcard. Run each full name from the
red-test record or use the complete `review_p` group.

### M1 item register

| ID | Status | Deliverable / acceptance |
| --- | --- | --- |
| M1.0 | Verified — E-08 | Engine-scoped DER roots supplement system trust. Spawned/manual, malformed DER, hostname/expiry, multiple roots and isolation tests pass on Windows x64/x86, Linux and both Macs; remote stable/MSRV. Separate public HTTPS checks prove unchanged system trust with/without a private CA. Android nonempty-root limitation documented. |
| M1.1 | Verified — E-08 | Unpublished meter tracks live requested Rust heap, exact logical peak and churn. Five tests cover live/peak retention, realloc, failures, simultaneous ownership and racing phase resets/snapshots. Separate plain binaries preserve uninstrumented performance observations. |
| M1.2 | Verified — E-08 | `tools/f5-observe/memory` leaves existing v1/v0.1.1 observer bytes unchanged. Separate fixture and one submitting client thread. All final workloads prove bytes, actual concurrency, reuse, cancellation, queue release and joined shutdown. |
| M1.3 | Verified — E-08 | Bounded controller records source/toolchain/config, process RAM, CPU, latency, throughput and fixture overhead. Real corruption and timeout tests invalidate output and join children. All 276 valid cases exit without forced cleanup. |
| M1.4 | Verified — E-08 | Final source C: Windows/Linux full 1/8/50 KiB × 1/16/32 HTTP/HTTPS matrix and extended workloads, each plain/meter × 3 repeats; 120 cases per host. Windows x86 and both Macs add 12 cases each. Report, raw evidence and M2 priorities retained. |
| M1.5 | Verified — E-08 | Focused 512 KiB TLS regression reproduces failure from partial-record carry; new 34 KiB logical plaintext limit accounts for one carried 16 KiB record plus the 18 KiB input window. Socket allowance/backpressure unchanged. Old 64 KiB and new regression prove bytes/bounds; all platform verifiers and extended HTTPS workloads pass. Vec capacity is measured separately from logical length. |

M1 implementation decisions: preserve all product defaults; the concurrency experiment explicitly
raises per-origin active/idle capacity to 32 so the default 8/4 limits do not silently turn a
32-connection case into queued work. Keep fixture allocations, certificate generation and server
threads in a separate process. Driver/request/response ownership remains included in client RAM
and is reported as such. The observer uses public production APIs and keeps unsafe allocator
forwarding in a small unpublished tool dependency; NBReq remains untouched.

The initial idea of disabling verification for the local TLS fixture is superseded by M1.0 after
the user identified the missing private-CA API as a valid-use-case blocker. The pinned
`rustls-platform-verifier` 0.7.0 provides `Verifier::new_with_extra_roots` on Windows, Linux and
Apple targets. `EngineConfig::with_additional_tls_root_certificate` now owns shared immutable DER
bytes, and Engine construction validates them through that same verifier. Defaults still take
the original path. Trust applies to the Engine and its redirects/pool/sessions; no dynamic trust
mutation, OS-store change, custom verifier callback or PEM convenience layer is introduced.
The pinned Android verifier lacks extra-root support; nonempty input explicitly returns Unsupported.
Local fixture tests will use a fresh private CA with full verification; separate bounded public
HTTPS observations verify system trust still works with an additional CA. All process maxima
are labeled by sampling/OS semantics; Rust heap sizes remain distinct from native allocations,
allocator overhead, stacks and process RAM.

M1 development history below is superseded by final E-08 acceptance. `target/m1/meter-red.log` has three intended peak failures (1088/4096/16384
bytes); `meter-green.log` has all three passing after peak tracking. `tls-roots-red.log` records
unknown issuer / ignored malformed-DER failures after adding configuration storage but before
wiring the verifier; `tls-roots-green.log` has four passing public-API tests. Current M1 changes
include `src/types.rs`, the two platform-TLS factory entry points, `native_tls.rs`,
`tests/tls_roots.rs`, private-CA guide text, and the unpublished memory meter. No baseline has
been measured yet. The user removed the old Linode M0 lab during further cleanup; prepare a fresh
M1 lab (source/logs remain local). Linux has about 28G available and stable/1.85 toolchains.

M1 progress, source B: four trust-root tests pass on Windows/Linux. The Macs correctly reject
expiry using CertificateInvalid because the pinned Apple verifier does not map the expiry code;
the test now checks that platform-specific result. Five meter tests pass, including failed
alloc/realloc and concurrent reset/snapshot checks. Windows controller tests pass for corruption,
timeout/child joins, exact success/reuse/concurrency, and public system trust with/without the
private CA. The four-case smoke passes after M1.5. Raw intended carry red/green logs are
`target/m1/tls-carry-red.log` and `tls-carry-green.log`; no invalid smoke is baseline evidence.

Frozen source B archive SHA-256: `6f9ea8d538c34b8e6f2a821bf7ede03ecb8f66fb4b197556804e76efd58bf852`.
Manifest SHA-256: `fd3fe46b898f296aa7402b1eb1511805ce47496d07c657a62263e759efc0cdf9` (114 source/script files
plus the manifest). New observer reproduction/methodology: `tools/f5-observe/memory/README.md`.
Full baselines are not complete yet. Existing legacy F5 main/README bytes are preserved.

Source C supersedes B for final observations. Its archive SHA-256 is
`d7707cc6bb170f3fafc64999c6642976fddb4b0c65e53b243bf12046a7d26a1f`; manifest SHA-256 is
`36886e7d54643a8aa60cae9c6f9f099c90a44d65d3dee246ae3b56334e1aa4ed`. Only the two generated
certificate fixtures differ: fresh 31-day leaves with explicit serverAuth EKU replace legacy
or overly long validity. The B observer was correctly rejected by both Macs under current
[Apple TLS certificate policy](https://support.apple.com/en-us/103769). No verification bypass
or product change was made for this fixture correction. C jobs check the two-file diff and
verify the manifest before/after measuring; they reuse B build caches only after B exits.

Windows B baseline completed 120 x64 and 12 x86 cases; it remains preliminary, superseded by C.
Final-product Windows verifier initially hit a DNS fixture UDP reset (10054), recorded in
`target/m1/windows-verify-b.log`; unchanged isolated rerun `windows-verify-c.log` passes all 24
steps. No test was skipped or weakened. Track that intermittent fixture failure separately if
it recurs; the successful baseline uses no DNS network fixture and no concurrent builds/tests.

## 4. Candidate profile — measurements still required

| Resource | Starting experiment |
| --- | --- |
| HTTP connection capacity | Keep 32; size per-origin capacity for long polls plus ordinary calls. |
| Accepted HTTP requests | Compare 64 against 1,024; align command/callback capacity with actual admission. |
| Idle pooled connections | Compare 4–8. Active long polls are not idle pool entries. |
| Ordinary request/response ceiling | Compare 128–256 KiB each, with explicit larger-operation handling. |
| Streaming/TCP window | Compare 16 and 32 KiB per direction; preserve nonblocking chunk-size contracts. |
| Shared streaming/TCP queue budget | Compare 1–2 MiB. |
| Buffered HTTP budget | Design around an initial 4–8 MiB range; this facility does not exist yet. |

Thirty-two TCP connections with two 16 KiB windows reserve 1 MiB rather than 16 MiB with 256 KiB
windows. That is not total process RAM. HTTP and standalone TCP connection ceilings are distinct;
measure the combination actually used. GDS can reduce pressure by admitting less work at once.

GDS currently permits 24 MiB HTTP bodies to accommodate large WebRPC frames. M2.5 now checks its
smaller request-specific response limits after completion but before consuming conversion.
Early enforcement remains M3; changing permitted frame sizes is a compatibility decision
owned by the handoff.

## 5. Open design questions

| ID | Question / required decision | When / owner | Status |
| --- | --- | --- | --- |
| MQ-01 | How is verification offloaded with bounded workers/job data, cancellation, and joined shutdown? Timing out a caller cannot be assumed to free an executing platform verifier. | M0.1 / NBReq | Accepted — MD-06; E-04 |
| MQ-02 | Which request, queued completion, returned response, and shared-body allocations stay charged? Define ownership transfer and any consuming/shared API before changing accounting. | M2/M3 / NBReq | Accepted — MD-13; Option C below |
| MQ-03 | How is buffered capacity reserved and exhaustion reported? Prevent all partial bodies from holding the budget while waiting for more space; use sufficient advance reservation or explicit bounded failure. | M3 / NBReq | Accepted — Option C, MD-16 |
| MQ-04 | Which large GDS operations are required, and should constrained devices split, stream, or explicitly reject them? Coordinate HTTP, encoded, decoded, and plaintext ceilings. | Before relevant M4 limits / GDS handoff | MD-20 implements initial admission while retaining 24 MiB body compatibility. Splitting/streaming and constrained-device exceptional-operation policy remain deferred. |
| MQ-05 | What total networking footprint and performance thresholds are acceptable on each target, after fixture and application memory are separated? | M1, finalize M4 / NBReq + GDS measurements | E-08 baseline available; application/target-device thresholds remain Open |

Post-M3 owner clarification: 50–70 MB is normal comms-server usage, not a universal process
ceiling; a large install at 512 MB is manageable. Preserve the 24 MiB body ceilings and use
installation-appropriate admission to avoid pressure. A connection can retain an upload,
response, staging and decoded application data, so 24 MiB does not bound its total RAM. M3
tests deliberately exhaust configured body budgets and prove typed failure/release/progress;
they do not establish recovery from arbitrary process/OS allocation failure. GDS workload
admission, byte-based queue control and any eviction policy remain separate M4/handoff work.

MQ-02–MQ-05 do not block M0. Preserve decisions with their rationale; if one changes, supersede it
explicitly rather than silently rewriting the history.

### MQ-01 accepted design — MD-06

The current `NativeTls::receive` calls Rustls `process_new_packets` on the reactor. The pinned
Rustls 0.23.42 verifier returns a synchronous `Result`; it has no pending/resume result, and packet
processing errors are fatal. Therefore move a bounded **handshake processing step** off the
reactor, temporarily transferring exclusive TLS connection ownership. Do not block the reactor
waiting for the worker. Socket readiness, deadlines, terminal arbitration, application bodies,
and user callbacks remain outside the worker; established TLS traffic keeps its current path.

Accepted contract:

1. Use an Engine-owned service with at most **two workers and four queued jobs**, started only
   when needed. These are initial engineering values, not measured defaults. One worker saves
   overhead but lets one slow check hold up every new handshake. Do not use per-request threads,
   the user callback pool, or a process-global pool whose lifetime exceeds Engine shutdown.
2. Bound input/output and queued/completed job storage. Preserve the existing 512 KiB handshake
   input ceiling; it is not a total TLS allocation bound. Pause socket reads when a handshake has
   no processing capacity, and keep waiting connections under existing admission/deadline limits.
   Job limits do not replace connection limits. Account for TLS internals and worker stacks in M1.
3. Separate `NativeTls`'s pending HTTP bytes from the transferable TLS state. A blocked job must
   not retain a cancelled request's potentially large body. Each job retains its resource charge
   until its owned data is actually released, including cancellation and undelivered results.
4. Cancelling a queued job removes it before verification begins. Cancelling or timing out an
   executing job promptly terminates the request and closes its NBReq socket, but cannot be
   assumed to interrupt the platform call. Discard late results using job/request identity;
   never deliver twice or recycle occupied capacity by spawning replacement workers.
5. Saturation delays new handshakes within bounded admission; existing HTTP/TCP/established TLS
   must continue. If both platform checks stall, new verified connections wait until capacity
   returns or their deadlines expire. This is the deliberate remaining isolation limit.
6. Shutdown stops admission, discards queued work, and closes NBReq sockets before waiting for
   executing checks and joining workers. Completion delivery must not deadlock worker joins.
   The existing `shutdown_for` duration bounds callback draining **after** network shutdown;
   it is not a hard deadline on native verification. Preserve DLL unload safety: no detached
   verifier may continue executing NBReq code after shutdown reports completion.
7. Keep certificate policy/error classification and manual Engine semantics: workers process
   supplied TLS bytes but do not own or poll NBReq sockets; completion wakes the owner for its
   next drive. Do not introduce early application transmission before verification succeeds.

The Windows verifier dependency sets a 10-second cumulative revocation retrieval timeout. That
is not proof of a universal 10-second bound for all platform verification or joined shutdown.
A hard shutdown deadline would need a separately justified cancellable provider or process
isolation design; neither is proposed for this correctness fix.

Companion proof before fixing: gate all workers and prove unrelated traffic progresses; cancel
queued work without invoking verification; cancel/time out executing work and observe socket
closure, body release, retained job charges, and ignored late completion; saturate/cancel/replace
without exceeding job/buffer limits; begin shutdown while gated and prove sockets close before
release and the shutdown joins after release. Cover manual driving and existing valid/invalid
TLS behavior as well. Use controlled gates with reliable cleanup, not live revocation URLs or
whole-process RAM assertions. E-04 records the design inspection; E-05 records the subsequent
implementation and Windows red/green proof. Thread/allocator/platform overhead still needs M1.

### MQ-02 accepted ownership contract — MD-13, 2026-09-07

**Option C accepted by the user; the aggregate budget is not implemented yet.** MQ-02 chooses
which allocations the future buffered-body budget owns and when that responsibility ends.
MQ-03 separately chooses advance reservation, allocation growth and exhaustion/progress behavior.

Source inspection before M2 implementation established:

* `Request` and `Response` owned `Vec<u8>` bodies; derived `Clone` copied those bytes.
  `RequestState::wait`, `wait_for`, manual polling and callback-job creation clone `Completion`,
  so delivery copies the response body. The public waiter is consumed, and callback requests
  have one callback; neither requires multiple payload deliveries. Keep terminal arbitration
  separate from payload storage so moving the result cannot reopen the terminal race.
* `RequestHandle` contains a Client and ID, not a per-request result reference. It does not by
  itself retain a completed response after registry removal. An unread `PendingRequest` does
  retain its result. Direct-waiter inflight admission is already refunded at terminal commit,
  so a count limit alone cannot bound a succession of unread completed bodies.
* Pending DNS/connect work retains both the original Request and serialized bytes including
  its body; HTTP/TLS setup clones that serialization at several handoffs. Redirects can copy
  the original body too. Preserve replay for allowed redirects and serial address fallback
  while removing duplication; a sent upload is not necessarily disposable yet.
* GDS's `execute_text` and `nbreq_response_to_dp` copy `response.body().to_vec()` before text
  conversion. The latter's smaller response limit is applied after that copy. These are source
  findings, not a measured attribution of every M1 allocation to one call site.

Options:

| Option | Charge ends | Benefit | Cost / limitation |
| --- | --- | --- | --- |
| A — active exchange only | Network terminal state | Simplest lifecycle accounting | Unread results and queued callbacks can retain bodies after capacity is refunded. Reject: this does not bound NBReq's own retained payloads. |
| B — delivery boundary | Waiter/manual return or callback invocation | Simple owned Vec response, straightforward no-copy `into_body`; no public body wrapper needed | Returned/retained responses are immediately application-owned and uncharged. A defensible Engine-internal budget, but ordinary collection of results bypasses it without an explicit transfer step. |
| C — retained body with explicit transfer | Last NBReq body owner drops, or unique storage transfers explicitly to application ownership | Covers unread and returned results; shared bodies count once; permits no-copy GDS conversion | Adds a small body owner/accounting object. Retaining replies can exhaust the budget; shared storage cannot always yield a Vec without copying. **Accepted — MD-13.** |

Accepted contract:

1. **Scope is buffered payload storage, not total Engine/process RAM.** Charge allocated capacity
   of admitted request bodies, buffered response bodies and additional body-bearing serialized
   or staging buffers. Separate copies each count; aliases to one backing allocation count once.
   Account for spare capacity, not just body length. Metadata-only headers/URLs/control objects
   remain under their existing limits and measured overhead allowance; TLS, sockets, thread
   stacks, allocator overhead and application allocations also need headroom. Do not label
   this a cap on all networking RAM.
2. **Acceptance adopts request storage into the Engine budget.** Caller construction and clones
   before submission are outside it. Insufficient capacity rejects acceptance under MQ-03;
   current submission consumes the Request even on error, so do not promise its return without
   an explicit API change. After acceptance, pending commands, DNS/connect waits, upload and
   redirect/replay retention all keep the appropriate charge until storage is released.
3. **Network completion does not refund retained response storage.** Queued callbacks, unread
   waiters, returned Response values and retained/cloned body owners stay charged. Moving a
   result between these stages moves ownership without copying or releasing its charge. A
   completed request's count permit and its retained-body byte charge have different lifetimes.
4. **Share immutable response bodies when a caller explicitly clones them.** Preserve the
   existing `Response::body() -> &[u8]` accessor. Response/Completion clones share the body
   backing allocation and one charge; metadata may still clone. Deliver by moving the payload,
   leaving a terminal marker, rather than retaining a hidden canonical body alias. Cancellation
   handles stay independent. Request replay can use moves or internal sharing; this proposal
   does not require a general public shared-request-body API.
5. **Offer explicit application ownership without a hidden payload copy.** Implemented API shape:
   `Response::into_body(self) -> ResponseBody` retains the charge;
   `ResponseBody::try_into_vec(self) -> Result<Vec<u8>, Self>` transfers the original Vec and
   ends the NBReq charge only when storage is uniquely owned. On sharing, return the original
   body owner unchanged and charged. The caller can drop other aliases or explicitly copy the
   borrowed bytes into its own Vec. A copy does not refund the original allocation. Do not
   silently copy on extraction or detach a charge while other NBReq aliases still retain it.
   This distinguishes destruction/refund from a documented ownership transfer: transferring
   a Vec does not free RAM, and the application must budget its resulting Vec/String/data.
6. **Keep the underlying Vec movable.** For example, a small reference-counted owner can contain
   the Vec and eventual permit; adopting existing bytes must not itself copy the body into a
   new slice allocation. API names/layout remain subject to implementation review. Cloned
   bodies retain the originating Engine's ledger. Bodies created by public `Response::new`
   outside an Engine are application-supplied and have no Engine charge; accepting an owned
   request into an Engine establishes its charge. Do not add cross-Engine budget-transfer or
   slicing/mutable-sharing APIs without a demonstrated need.
7. **Release follows physical ownership, including cancellation.** Free charged storage before
   refund on destruction; retain charges for delayed cleanup/late results, shared owners, and
   any retained allocation caches. Explicit unique Vec transfer is the exception above. A body
   may outlive Engine shutdown through a small independent ledger, but must not keep reactors,
   workers or sockets alive or make shutdown wait for caller data. Existing DLL unload rules
   still apply to caller-held objects; this is not permission to unload code while it is used.

For GDS, the ordinary un-cloned result should reach conversion with a unique body. Consuming
extraction then lets UTF-8 String construction reuse the Vec storage. Check existing status/size
rules before conversion; M3 will additionally enforce request-specific body limits during the
exchange. An explicit copy remains available for deliberately shared bodies. No normal request
should fail merely because an internal completion alias was accidentally retained. Broader GDS
queue/encoding budgets still belong to the handoff, including bytes after successful extraction.

The cost is a small ownership object and reference-count/ledger operations; sharing is not free.
Measure the 1 KiB path as well as the larger cases before accepting a performance claim. The
benefit sought is removal of payload-sized copies and honest retained-body accounting. Keeping
old responses may prevent a new request from acquiring capacity; MQ-03 must make that bounded
and observable, never an indefinite wait that requires the same caller to release a body it
cannot reach. Option B remains a reasonable simpler alternative if tracking returned bodies is
not wanted. Do not add selectable B/C budget modes by default.

TDD sequence:

* M2: prove one payload delivery without a body copy for spawned wait, timed wait and manual
  drive, plus callbacks; retain exact-once terminal/cancellation/activation/shutdown behavior.
  Verify a held cancellation handle does not create a hidden payload owner. Prove unique
  consuming extraction preserves storage, sharing preserves bytes, and failed extraction
  returns the original owner without copying. Preserve request bytes over allowed redirects
  and serial address fallback while removing serialization copies.
* M3: use deterministic capacity/accounting assertions for accepted request storage, spare
  capacity, queued/retained results, separate copies and shared aliases. Test drop ordering,
  late cancellation cleanup, explicit transfer, shared extraction failure, last-owner release,
  Engine isolation and retained bodies after joined shutdown. Add growth/reservation and
  exhaustion/progress tests with MQ-03. Do not substitute RSS assertions for ownership proof.
* Repeat relevant M1 ordinary/large-transfer/churn cases after changes, using each host's own
  baseline. Linode's single CPU is shared by fixture/controller/client, unlike the Windows
  host; the current measurements cannot isolate an OS/backend performance difference.

### MQ-03 decision — 2026-09-08, accepted Option C (MD-16)

MQ-02 already fixes ownership and charge lifetime. This proposal chooses how capacity is
acquired and what happens when insufficient capacity remains. It does not select GDS's final
budget value or change any valid protocol ceiling.

| Option | Advantage | Cost / failure mode |
| --- | --- | --- |
| A — reserve each request's full response allowance before submission | Can reject insufficient aggregate capacity before network transmission; reserve staging/growth headroom too for a meaningful body-budget guarantee. Reservation need not allocate the RAM immediately. | Very conservative with permissive ceilings: 32 requests with a 24 MiB response allowance need 768 MiB of response reservation alone, even when actual replies are tiny. Tight per-operation ceilings or lower concurrency become essential. |
| B — reserve incrementally and wait for more budget | Uses budget according to actual growth and can absorb temporary contention. | Fully buffered replies are not exposed until complete. Several partial replies can occupy all capacity while each waits for more; retained caller responses can cause the same circular dependency. Deadlines bound some waits but do not provide useful progress. Do not use as the initial policy. |
| C — reserve according to known size/growth, fail explicitly if unavailable | Supports small replies under large legal ceilings; bounds acquisition and avoids waiting on partial bodies. Little work on the ordinary path if accounting occurs per allocation/growth. Recommended initial policy. | Acceptance does not guarantee response capacity through completion. A request may fail after the server has acted. Contention is timing-dependent, and repeated pressure can starve large operations; GDS must control admission. |

Accepted contract for C:

1. Make the new aggregate cap explicitly configured initially. Preserve existing default
   behavior until a measured profile is selected. It covers the MQ-02 buffered payload scope,
   independently of per-request body limits and the existing streaming/TCP queue budget.
2. Before accepting a request, acquire capacity for its existing body allocation and any
   required body staging. If unavailable, reject before queueing/transmission. Do not reserve
   the entire maximum response allowance for every pending request or quiet long poll.
3. When validated HTTP framing establishes the actual response length, first enforce the
   effective per-request ceiling, then reserve the required response capacity before body
   allocation. Reservations are permission, not mandatory immediate allocation. HEAD/no-body
   responses must not reserve a body merely because Content-Length advertises a representation.
4. For chunked/close-delimited responses, acquire capacity before bounded growth. Failure to
   acquire terminates that exchange with a distinct typed aggregate-budget reason; it does not
   park an incomplete body waiting for capacity or automatically replay the HTTP operation.
   Dispose of partial bodies and unusable connections correctly; unrelated work continues.
5. Preserve MQ-02 lifetimes: retained results stay charged, aliases count once, destruction
   frees storage before refund, and explicit unique transfer hands ownership to the caller.
   Unused advance reservations may be returned without pretending retained storage was freed.
6. Account for spare capacity and transient old/new-buffer overlap during growth. Avoid a
   per-byte lock or repeated tiny reallocations. The implementation must justify how allocation
   sizes are bounded; merely checking body length or invoking Vec reserve is insufficient.
   TLS, sockets, stacks, allocator overhead and application-owned data remain separate headroom.
7. Distinguish aggregate exhaustion from per-request oversize. Do not classify every exhaustion
   as safe to retry: response-side failure can follow a successful server-side POST. Current
   GDS HTTP retry classification excludes Limit errors; retain that behavior unless its
   application delivery contract explicitly justifies a change.

The principal trade-off is memory utilization versus a pre-send response-capacity guarantee.
If a caller needs the latter, a deliberately requested full response reservation is a reasonable
follow-up; it should not silently become the price of every ordinary request. GDS scheduling
and known small reply ceilings may already cover that need. No extra reservation mode is
proposed for the first implementation without a concrete consuming call site.

The candidate 4–8 MiB budget remains an experiment for ordinary traffic. It cannot buffer a
single 24 MiB response. Supporting such operations needs a larger budget, streaming/splitting,
or an explicit constrained-device policy under MQ-04; C does not remove that physical limit.

TDD first: deterministic pre-admission failure with no wire I/O; known-length oversize/capacity
rejection; multiple chunked replies exhausting a tiny budget without hanging; exact cleanup and
subsequent progress; retained/shared replies, transfer, cancellation and joined shutdown;
growth-overlap/spare-capacity accounting; a POST whose server has acted before response-budget
failure, proving no hidden retry or duplicate delivery. Reuse the M1 small/large measurement
cases to assess accounting/growth overhead. Implement working controls first; choose final GDS
values afterwards. The user reports that GDS generally limits replies to roughly 1 MiB because
large Delphi allocations and fragmentation are troublesome. This is tuning context, not an
instruction to replace the existing 24 MiB exceptional-operation ceilings with a hard 1 MiB cap.

### M3 item register

| ID | Status | Deliverable / acceptance |
| --- | --- | --- |
| M3.1 | Verified — E-13 | Optional request/response ceilings bounded by Engine ceilings; uploads fail before admission, replies during framing/receipt. Shared buffered/streamed request options preserve redirects and HEAD/no-body. GDS per-request pass-through proved red/green. |
| M3.2 | Verified — E-13 | Independent retained-capacity ledger and optional aggregate knob. Spare capacity, concurrent admission, shared lifetime after shutdown, unique transfer and queued uploads tested. |
| M3.3 | Verified — E-13 | Known lengths reserve without immediate allocation; bounded growth charges old/new overlap. Receive events/TLS plaintext retain permits. Cleartext upload windows share original storage. Typed pressure, cleanup, later progress and worker cancellation tested. |
| M3.4 | Verified — E-13 | Current/peak observations and GDS startup controls documented. Final E passes seven full gates, seventeen x86 companions, native/ureq GDS checks, 120 memory cases, 36 longer timing cases and four configured-cap probes. Earlier rejected fixture/harness attempts remain visible. Final numerical profile remains M4. |

References: [Rust Vec allocation guarantees](https://doc.rust-lang.org/std/vec/struct.Vec.html#method.try_reserve_exact)
do not promise precisely minimal capacity; [HTTP retry semantics](https://www.rfc-editor.org/rfc/rfc9110.html#section-9.2.2)
require particular care when a non-idempotent operation might already have been applied.

## 6. Verification matrix and evidence

| Gate | Required evidence | Current state |
| --- | --- | --- |
| V0 — regression proof | Intended failures for fourteen review tests, with other tests passing. | Proven on Windows; E-01. |
| V1 — correctness completion | Focused fixes, companion races/lifetimes, all regressions, formatting/lints, and normal verifier without skips. | Complete for M0 on Windows x64/x86, Linux x64, and both Macs; E-06/E-07. |
| V2 — allocation/accounting | Fixed/chunked early oversize rejection; aggregate exhaustion; retained results; cancellation/drop release; bounded retained capacity. Use deterministic tests. | Planned M2/M3. |
| V3 — platform/feature coverage | Windows/Linux memory observations plus the project's applicable feature and supported-platform correctness gates. Record exact source/build and unrun targets. | E-08 correctness gates pass; full Windows/Linux memory matrix plus useful x86/Mac cases complete. Actual 128/256 MB devices and GDS integration await M4. |
| V4 — representative workload | 1/8/50 KiB messages at 1/16/32 connections; cold/warm TLS; long polls plus bursts; slow peers/readers; cancellation/replacement; a large transfer followed by idle. | E-08 establishes the complete M1 baseline; repeat relevant cases during M2–M4 changes. |
| V5 — performance and quiescence | Peak live allocations and process RAM, allocation churn, latency, throughput, CPU, joined shutdown, and post-burst idle retention. Separate fixture overhead. | E-08: 276 valid final cases with source/toolchain/config and separate fixture observations; exact logical heap and sampled OS counters are distinguished. |

| Evidence ID | Record | Scope and interpretation |
| --- | --- | --- |
| E-01 | [Red-test record](nbreq_review_red_tests.md), 2026-09-05 | Historical baseline: thirteen library failures plus one packaging failure; remaining 330 library, 26 integration, and 26 doctests pass. Native-only had eleven applicable library regressions. No fixes were proven at that point. |
| E-02 | [F5 observer](../tools/f5-observe/README.md); saved `target/f5-observe/results/20260904T060619Z-currentnative-plain.json` | Prior x64 Windows, one active connection, 128-byte HTTP, in-process fixture: about 8.2 MiB peak working set and 1.6 MiB sampled private bytes. Not concurrent HTTPS/GDS evidence. Cumulative allocation totals do not measure peak live memory. The ignored raw artifact may need regeneration. |
| E-03 | [GDS handoff](gds_memory_handoff.md), source inspection 2026-09-05 | Adapter copies, late limits, 24/18/16 MiB WebRPC ceilings, and count-based queues. The inspected GDS manifest pins 0.1.0; verify actual checkout/build before using 0.2 APIs or claiming deployed behavior. |
| E-04 | MQ-01 source inspection, 2026-09-05: [TLS path](../src/backend/native_tls.rs), [shutdown](../src/engine.rs), [locked dependencies](../Cargo.lock); local Rustls 0.23.42 `src/verify.rs` / `src/conn.rs` and rustls-platform-verifier 0.7.0 `src/verification/windows.rs`; [Microsoft retrieval timeout semantics](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/nf-wincrypt-certgetcertificatechain) | Same HEAD/dirty tree as checkpoint, inspected on Windows; no build or test run. Synchronous verification, fatal packet errors, request bytes inside `NativeTls`, and callback-only `shutdown_for` duration establish the design constraints. Worker counts/performance and platform completion time are not measured. |
| E-05 | M0.1 implementation and verification, 2026-09-05; details below | Windows x64 Rust 1.97.1, debug/test builds, HEAD `b4c4d74cea0e` plus dirty source. TLS P1 and 11 companions pass; remaining P2 failures preserved. Windows x86 all-target compilation passes. No Linux runtime or memory/performance proof. |
| E-06 | M0.2–M0.9 fixes and final M0 verification, 2026-09-05; details below | Rust 1.97.1, Windows x64 and i686 test execution. All fourteen original regressions and 23 companions pass. Normal x64 verifier: 24/24 steps. Extracted crate: 366 library tests with local support overrides. No Linux, other-platform, registry-only release, or memory/performance proof. |
| E-07 | M0 bridge-host validation and memory-tool readiness, 2026-09-05; details below | Same snapshot B on Linux x64 and physical Intel/Apple Silicon Macs: all six stable/MSRV verifiers 24/24, Mac support tests/lint, extracted-package suites, F5 modes, and trusted HTTPS pass. Portable fixture retains mutation proof. `nbreq-darwin` publication is a release prerequisite. No representative memory/performance claim. |
| E-08 | [M1 baseline and recommendations](nbreq_m1_baseline.md), 2026-09-05; [raw evidence](evidence/nbreq-m1-evidence-20260905.tar.gz), [source C](evidence/nbreq-m1-source-c.tar.gz), [aggregate data](evidence/nbreq_m1_summary.json) | M1.0–M1.5 verified and M1 accepted. Private CA support and TLS carry fix, focused red/green tests, Windows + six remote full verifiers, final fixture checks, and 276 valid observations. Ordinary 32×50 KiB HTTPS peaks: Windows 10.03 MiB private / 19.68 MiB working set; Linux 11.61 MiB RSS; Rust heap 5.03 / 5.65 MiB. Extended peaks and caveats are in the report. Whole GDS/device fit remains unproven. |
| E-09 | [M2.1/M2.2 report](nbreq_m2_response_ownership.md), 2026-09-07; [evidence](evidence/nbreq-m2-response-evidence-20260907.tar.gz), [source](evidence/nbreq-m2-response-20260907.tar.gz), [receipt](evidence/nbreq_m2_response_artifacts.json) | MQ-02 API and moved terminal delivery: thirteen ownership companions, Windows/x86 checks, all six current-source remote stable/MSRV verifiers and 120 valid paired Windows/Linux cases. Roughly 50 KiB less cumulative allocation per 50 KiB response; large-transfer peak remains about 14.4 MiB. Stale-cache attempts and an ARM first failure/rechecks are retained separately. |
| E-10 | [M2.3 request report](nbreq_m23_request_ownership.md), [artifact identities](evidence/nbreq_m23_request_artifacts.json), [evidence](evidence/nbreq-m23-request-evidence-20260907.tar.gz), 2026-09-07 | M2.3 verified. Seven final full verifiers, ten memory tests, x86, shutdown/DNS rechecks, 120 paired cases and 20 timing follow-ups pass. Six ownership reds, TLS-record red and shutdown-order red retained; all failed attempts documented. Final D differs from measured C only in the DNS test fixture. Large-transfer heap peaks improve about 27% Windows/17% Linux; retention and performance limits are explicit. |
| E-11 | [M2.4 retained-capacity report](nbreq_m24_retained_capacity.md), [artifact identities](evidence/nbreq_m24_retention_artifacts.json), [evidence](evidence/nbreq-m24-retention-evidence-20260908.tar.gz), 2026-09-08 | M2.4 verified: three intended reds, six companions, seven full verifiers, 29 x86 M2 tests, 120 paired cases and 30 longer timing cases. A idle-eviction fixture failures preserved; B corrects synchronization only. Mixed-workload post-large idle heap nearly halves on both measured hosts; ordinary churn/retention and performance limits are explicit. |
| E-12 | [M2.5 GDS report](nbreq_m25_gds_conversion.md), [artifact identities](evidence/nbreq_m25_artifacts.json), 2026-09-08 | Verified: three allocation reds, eight new tests; x86 HTTP/WebRPC and native/ureq builds; Linux exact adapter; both Mac stable/MSRV lower-patch tests. Public Mac source/evidence and 89-file private GDS evidence archived. No deployment or registry publication. |
| E-13 | [M3 controls and measurements](nbreq_m3_memory_controls.md), [artifact identities](evidence/nbreq_m3_artifacts.json), [evidence](evidence/nbreq-m3-memory-evidence-20260908.tar.gz), 2026-09-08 | M3 accepted: five source snapshots, 2,696 public evidence files and 45 private GDS files. Final E seven full gates, seventeen x86 companions, 120 memory cases, 36 longer timings, four cap probes and native/ureq GDS checks pass. Per-body defaults unchanged; final aggregate/GDS settings remain open. |
| E-14 | [M4.1 GDS admission](nbreq_m4_gds_admission.md), [artifact identities](evidence/nbreq_m4_artifacts.json), 2026-09-08 | Final production B verified: seven runtime reds resolved; x86 native HTTP/WebRPC 17/90 and ureq-only 2/89 tests pass; both DLL builds pass with SkipCopy. Nine portable policy tests pass on x86 and Linux stable/MSRV. Corrected C component timings use unchanged B policy; A/B timing is superseded. 191 private source/evidence files archived and checked in GDS. All jobs finished; installed DLL and nbreq M3 E source unchanged. Full GDS/device memory and pickup-latency acceptance remain M4. |

For each new evidence entry record date, work ID, exact source or dirty-diff identity, target,
features/build mode, command, result, artifact, and limitations. A failed correctness or quiescence
check invalidates that run as performance evidence. Use F5 for measurements; do not turn noisy
whole-process RAM observations into ordinary unit-test assertions.

The standard local correctness entry point is `cargo run --manifest-path tools/xtask/Cargo.toml -- verify --offline`.

### E-08 final memory baseline record

M1 is accepted for its measurement scope. Final C observations: Windows x64 and Linode each run
120 cases; Windows x86, Intel Mac and Apple Silicon Mac each run 12 additional cases. Three
repetitions, separate plain/meter binaries, exact-body checks, actual fixture concurrency,
normal-case reuse, cancellation/accounting, queue release and joined shutdown are required.
All final measurement children exit 0 without forced termination. Four controller integration
checks also pass on Windows/Linux/both Macs, including corruption, timeout cleanup and normal
system trust with/without the private root. Five meter tests and warning-denied lints pass.

Full production verification: Windows x64 24/24; Linux, Intel Mac and Apple Silicon stable 1.98
and MSRV 1.85 each 24/24. Source C differs from verified M1 source B only in the two certificate
fixtures, which have their own final stable/MSRV tests/lints. The final source manifest matches
all 114 files after measurements on each host and locally. Windows x86 runs the new roots,
streaming TLS and meter checks plus the representative workloads. All tests use normal verifier
policy; the current Apple certificate fixture requirements and broader expiry category are
recorded in the report. One Windows DNS fixture reset failure is retained alongside its unchanged
full green rerun. No failed fixture or benchmark run is represented as performance evidence.

The new TLS streaming regression fails before the fix with the exact bounded-plaintext Internal
error. A carried partial record can produce more plaintext than the latest socket read alone;
34 KiB logical retention accounts for the 18 KiB input and one 16 KiB carry. The 18 KiB socket
allowance and consumer backpressure remain. The new roots API supplements platform trust and
keeps each Engine's policy immutable; default engines do not inherit another Engine's roots.

The [baseline report](nbreq_m1_baseline.md) owns numbers, methods and limits. Small buffered calls
are substantially below the configured maxima; large buffered copies and full streaming queues
are the first measured pressure points. M2 starts with internal copies/serialization; MQ-02
precedes public consuming/shared body APIs. M3's early limits and aggregate budget remain needed.
No GDS implementation, memory defaults, allocator choice, host trust store or publication changed.

Final labs: `/home/ubuntu/nbreq-m1-20260905-c`, `/Users/andrew/nbreq-m1-20260905-c`, and
`/Users/m1/nbreq-m1-20260905-c`; each keeps its binaries/results and reuses the sibling `-b/target`
build cache. All jobs have finished. `target/m1` holds local development logs, but the source,
measurement data, verification logs and analysis scripts are also in the durable E-08 archives.
Use the observer README to reproduce; do not start another full run merely to rediscover this
baseline without a relevant code/configuration change or unresolved measurement concern.
Keep the project's additional platform/package gates explicit. Temporary `--skip review_p` runs
are diagnostic controls only and cannot satisfy V1.

### Bridge-host validation / E-07

**All three hosts pass both toolchain gates and readiness checks on final snapshot B.**
The normal verifier runs unchanged, without skip filters. Each Mac also passes all five
`nbreq-darwin` tests and warning-denied helper lint on both toolchains.

| Target | Stable 1.98 verifier | MSRV 1.85 verifier | All-feature library / integration / doctests, each toolchain | Extracted package, stable |
| --- | --- | --- | --- | --- |
| Linux x86_64 | 24/24 (329.711s) | 24/24 (363.026s) | 364 / 27 / 26 | 364 pass |
| Intel Mac x86_64 | 24/24 (164.865s) | 24/24 (166.024s) | 362 / 27 / 26 | 362 pass |
| Apple Silicon Mac aarch64 | 24/24 (82.337s) | 24/24 (82.689s) | 362 / 27 / 26 | 362 pass |

The preliminary Mac failure was a fixture assumption: Darwin rejected binding unconfigured
`127.0.0.2`. The [fallback fixture](../src/backend/native_http/tests.rs) now uses an invalid TCP
broadcast destination on Darwin and proves it fails without timing out before requiring the
healthy second DNS address to succeed. Windows/Linux retain the reserved non-listening endpoint.
No production code changed in this platform pass. Both Macs rejected a deliberate
first-address-only mutation at the intended fallback assertion in a disposable extracted crate;
the extracted source was restored. Windows targeted fallback, formatting, and all-target,
all-feature warning-denied lint also pass after this fixture change. Linux stable initially
lacked rustfmt/clippy; installing those standard components resolved that tooling gap.

Both release F5 binaries (`f5-observe-plain`, `f5-observe-alloc`) build and smoke successfully on
every host: one measured sample, two warmups, eight requests, 1 KiB responses. Each JSON reports
exact bytes/reuse, zero operation/queue gauges, expected keepalive state, joined shutdown, and
fixture cleanup. A separate native-platform HTTPS example returns HTTP 200 with 559 bytes on
every host. `/proc` and GNU time are available on Linux; time/vmmap/footprint are available on
both Macs. These checks establish readiness, **not representative memory or performance results**.
The in-process HTTP fixture and single active connection remain limitations of this smoke.

Online `cargo package --allow-dirty` fails on all three hosts because crates.io has no matching
`nbreq-darwin`. The owner confirmed its role as an unsafe-support crate, analogous to winpoll,
and its deferred publication process (MD-09). A temporary Cargo configuration points at the
local Darwin/winpoll support crates; the resulting normalized archive compiles and its all-feature
library suite passes on each host. This proves the packaged-fixture fix, not registry-only release
readiness. No manifest workaround was committed and nothing was published.

Source: NBReq 0.2.0, HEAD `b4c4d74cea0e` plus dirty work. Local archive
`target/m0-platform/nbreq-m0-platform-b.tar.gz`: 117 files, 380,892 bytes; SHA-256
`33d12e000606e25554670811c1d7f1b48edbc4f6c90d9e6b4a8bc5980294145a`.
Bundled `m0-bundle.sha256` SHA-256:
`9d7e724eb2cd2c47c2e5e3b791043e8e1ee28797fe976308dace09057f9cfccf`.
All product/test bytes except the fallback fixture match E-06. The snapshot also includes xtask
and existing F5/physical helpers; no GDS source or credentials. All 116 manifested files pass
checks before and after the remote runs. Preliminary `-a` labs are historical; use **`-b`** below.

Use the owner-controlled bridge documented in
`C:/User/SecuritasNew/gds/doc/ai_agents/PUTTY_PLINK_BRIDGE.md`. Bridge roots below are under
`C:/User/SecuritasNew/gds/scripts/sessions/`. All final run/warmup exit markers are zero, their
recorded PIDs have finished, and no lab process remains. Labs and caches are retained for M1.
At the end of verification, free disk was about 3.1 GiB on Linux, 64 GiB on Intel, and 179 GiB
on Apple Silicon. The subsequent authorized [Linode cleanup](nbreq_linode_cleanup_20260905.md)
removed 38 Cargo caches from before 2 September and recovered 24.20 GiB; Linux then had 27.44 GiB
free. Both M0 labs and the 2 September work remain intact; all 116 current source hashes were
rechecked. Old source snapshots and historical evidence remain listed for owner review, including
an older `/home/ubuntu/nbreq` checkout with uncommitted curl work.

| Host / bridge root | Observed host | Prepared lab |
| --- | --- | --- |
| Linux Linode / `putty_bridge` | Ubuntu 20.04.6, Linux 5.4.0-216 x86_64; about 2 GiB RAM. Stable 1.98 and 1.85 installed; default 1.85. | `/home/ubuntu/nbreq-m0-20260905-b` |
| Physical Intel Mac / `putty_bridge_intel_mac` | macOS 15.7.9 (24G830), x86_64, 8 GiB RAM, 4 CPUs. Stable 1.98 and 1.85. | `/Users/andrew/nbreq-m0-20260905-b` |
| Scaleway Apple Silicon / `putty_bridge_scaleway_nbreq` | macOS 26.6.1 (25G76), arm64, 8 GiB RAM, 8 CPUs. Stable 1.98 and 1.85. | `/Users/m1/nbreq-m0-20260905-b` |

Reproduction inside `<lab>/nbreq`, for each `RUSTUP_TOOLCHAIN=stable` and `1.85.0`:

```sh
cargo fetch --locked
cargo run --locked --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline
# Additionally on the Macs:
cargo test --locked --offline --manifest-path support/darwin/Cargo.toml
cargo clippy --locked --offline --manifest-path support/darwin/Cargo.toml --all-targets -- -D warnings
```

The runner sets `CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`, dev/test debug symbols to zero,
and a separate `<lab>/target-<toolchain>` directory per toolchain to bound Linux disk use.
These settings belong to correctness runs, not a chosen performance profile. Exact scripts are
in local `target/m0-platform/run-one.sh`, `run-platform.sh`, and `warmup.sh`; the first two are
also bundled as `m0-run-one.sh` / `m0-run-platform.sh`. Warmup script remote path:
`/tmp/nbreq-m0-20260905-warmup.sh`, SHA-256
`955c086226922c402d9c1c0403eca1f8bd0e6059bc1cff94cfb9293dde800793`.
Its source label is `b4c4d74cea0e+dirty-33d12e000606`; the script records the local support-crate
override and restores the disposable Mac mutant even on failure.

Evidence is saved remotely as `<lab>/m0-evidence.tar.gz` and locally under
`target/m0-platform/{linux,intel,arm}/`. This contains both verifier logs/exit codes, run and
warmup logs/markers, smoke JSON, package logs, and source hash checks. Local `summary.json` and
`final-state.json` record parsed counts and final remote state. Transport archive hashes match
the remote hashes:

| Local artifact under `target/m0-platform/` | SHA-256 |
| --- | --- |
| `linux/evidence.tar.gz` | `b3d3260641f83e68adf43c2865552816b15d8d7e0d0a3864bc77b1e03b540651` |
| `intel/evidence.tar.gz` | `390f2ecd33736db19e75282b072966eef3b701c59c121cf598189f02159e1bee` |
| `arm/evidence.tar.gz` | `cdd5b36893425e14baa524b4faa2201c3584db5bd9d522640ee738120e13b496` |

Artifacts under `target` are ignored and may need regeneration. Evidence covers these OS/CPU
hosts only; it does not establish 128/256 MB device fit, representative concurrency, peak live
allocations, or memory/performance changes. M1 remains the next implementation package.

Bridge resume notes: use the explicit correct root, queue only one outstanding request per
bridge, and check its result before another request. Mac keys were restored by the owner after
reboot. Do not bypass the bridge or supply approval tokens. Embedded double quotes can be lost
through Windows PowerShell/PuTTY argument handling, so use uploaded shell scripts for complex
commands. Require expected remote markers as well as a successful bridge result. Inspect status
and processes before retrying a detached runner; a missing exit marker means running/unknown.

### M0.2–M0.9 implementation / E-06

Historical Windows checkpoint: E-07 supersedes the platform and registry-status remainders below.

Original red failures were reproduced again before their fixes. HTTP now serially tries remaining
DNS addresses only until TCP succeeds, retaining one transfer, connection reservation, original
body allocation, and deadlines. The request is never replayed after TCP connects. DNS caches the
full wire-bounded answer (4 KiB packet limit and existing entry/TTL bounds); output caps apply at
delivery, including refresh and combined-family ordering.

Blocking TCP send waits for enough capacity for its whole chunk and wakes on sufficient write
progress or cancellation; cancellation preserves the unaccepted suffix. Abort drops all queue
and payload allocations before refunding admission, resets offsets/accounting, and stays
idempotent. Graceful FIN handling is unchanged. A permanent finish-registration flag enforces
single registration, including concurrent attempts and registration after delivery.

HTTP 205 uses ordinary message framing, consuming final chunks and trailers, respecting explicit
zero length or connection close, and retaining HEAD/204/304 exceptions. DNS ID allocation uses
four fresh OS-random bytes per allocation (start plus odd collision stride), visits at most the
65,536-ID space, preserves HTTP reservation, and fails closed when entropy is unavailable.
The wire-sequence test is a regression check, not a cryptographic proof.

M0.9 found that Cargo excludes the nested fuzz package even with explicit include patterns.
The 17 shared seeds now live under tests/fixtures/fuzz; the parser tests and documented fuzz
commands reference that one corpus. All moved files match their original Git bytes. The manifest
includes reviewed `.seed` files and excludes generated campaign corpus files. The final archive
contains 71 files (1.7 MiB unpacked, about 293 KiB compressed) and passes its unit suite. No fuzz
test campaign was run; the ordinary checked-in seed/oracle tests ran in every applicable suite.

| Command / gate | Recorded outcome |
| --- | --- |
| `cargo run --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline` | **24/24 steps pass**, no skip filters, 123.578 seconds. Formatting, warning-denied lint, feature matrix, documentation, wrapper tests, and all three pressure regressions pass. `target/m0-verify.log`. |
| All-feature/default suites within that verifier | 366 library, 27 integration, 26 doctests pass. The native-only suite has 314 library, 26 integration, 25 doctests; minimal has 106 library, 5 integration, 25 doctests. |
| `cargo test --offline --all-features --lib review_p -- --test-threads=1` | All 13 original library regressions pass. `target/m0-original-regressions.log`. |
| `cargo test --offline --all-features --test package_contents --quiet` | Original packaging regression passes using Cargo's actual inventory. `target/m0-package-inventory.log`. |
| `cargo test --offline --target i686-pc-windows-msvc --all-features` | **Execution**, not only compilation: 366 library, 27 integration, 26 doctests pass under WoW64. `target/m0-i686-tests.log`. |
| `cargo package --allow-dirty --offline --config target/m0-package-patches.toml` | Final archive built and Cargo's extracted-library verification passes. `target/m0-package-final.log`. Local support dependency overrides described below. |
| `cargo test --offline --manifest-path target/package/nbreq-0.2.0/Cargo.toml --config target/m0-package-patches.toml --all-features --lib` | All **366 library tests pass from the extracted final crate**, including compile-time fixture consumers. `target/m0-extracted-final-tests.log`. |

The normal verifier preceded the final two-line manifest exclusion for generated fuzz corpus.
Production/test source did not change afterward. The x86 suite, explicit original-regression and
inventory checks, final package build, and extracted suite include that exclusion. A transient
Windows mapped-file error interrupted the first formatting write; retry and verifier formatting
passed. No formatting or test failure remains outstanding.

Registry-only packaging could not be validated: offline Cargo lacks an indexed `nbreq-darwin`,
and the online attempt failed before resolution with Schannel `SEC_E_NO_CREDENTIALS`
(`target/m0-package.log`). This is an environment/dependency validation gap. The local check uses
this temporary configuration, preserving the normalized package manifest and all packaged source:

```toml
[patch.crates-io]
nbreq-darwin = { path = "C:/User/projects/nbreq/support/darwin" }
nbreq-winpoll = { path = "C:/User/projects/nbreq/support/winpoll" }
```

Source manifest `target/m0-source.sha256` covers 75 files: `src`, `tests` including shared seeds,
`support`, root Cargo files, README, the user guide, fuzz README, and the corpus ignore rules.
Manifest SHA-256: `b5c5986167358bd0cd33bedae0f0fbac21bbdb2184606687e80801ab64142bea`.
Final crate SHA-256: `1dffff42038a13043dc038ec98217c13f01b2047398048f5d7f116f7996ab6ba`.
These identify a dirty development tree, not a release commit. Ignored `target` artifacts and the
temporary override file may need regeneration. No commit or publication was made.

Linux and other supported-platform gates remain unrun. The prior WSL enumeration failed with
access denied; this Windows host provides no Linux execution evidence. M0 is accepted for the
stated Windows scope; carry those checks and registry-only package verification into release
validation. M1–M4, live-allocation/performance measurements, and GDS changes remain unstarted.

### M0.1 implementation / E-05

Historical checkpoint: E-06 above supersedes its remaining-red and x86-coverage status.

Production work is in [handshake workers](../src/backend/native_tls/worker.rs),
[TLS ownership](../src/backend/native_tls.rs), and [HTTP coordination](../src/backend/native_http.rs).
HTTP request bodies remain with the reactor when `TlsSession` moves to a worker. Cancellation
removes queued work or marks executing work for discard; tickets survive until owned job data is
released. Completed results consume the same six-ticket ceiling. Connections waiting outside the
four-job queue retain at most one 16 KiB encrypted input window each and remain under connection
admission and deadlines. All established TLS processing retains its previous owner path.

Shutdown first stops handshake admission and closes sockets, discards queued work, stops/joins DNS,
then joins executing TLS checks. No worker waits for result consumption during shutdown. Total and
connect deadlines are checked again before a queued step executes. Deferred FIN handling preserves
the order of received TLS bytes and peer closure. The [user guide](../docs/getting-started.md) and
README now state the platform-verification shutdown limitation.

A subsequent parallel run of the remaining red tests stalled in the fallback test and was stopped.
An isolated rerun exposed a Windows fixture error: an accepted socket inherited nonblocking mode,
so its timeout-based request read could fail with `WouldBlock`. The fixture now explicitly restores
blocking mode on accepted sockets. The isolated and parallel P2 runs then finished with the intended
fallback failure and all twelve expected library reds. This does not implement address fallback.
The stopped run is saved as `target/mq01-remaining-reds-parallel-hang.log`; the corrected isolated
proof is `target/mq01-fallback-isolated.log`.

Before implementation, the original P1 failed again with unrelated HTTP blocked. Three newly added
socket-lifecycle tests also failed for the intended reasons: cancellation retained its socket,
timeout waited for verification, and shutdown left its socket open. After the fix, these pass along
with [manual/saturation/FIN integration coverage](../src/backend/native_http/tests/tls_lifecycle.rs)
and [worker lifecycle/bounds coverage](../src/backend/native_tls/worker/tests.rs): 11 new tests total.
The body-retention test checks ownership and retained capacity, not whole-process RAM.

| Command / gate | Recorded outcome |
| --- | --- |
| `cargo test --offline --all-features --quiet -- --skip review_p2` | 342 library, 26 integration, 26 doctests pass; original TLS regression included. Log: `target/mq01-all-features.log`. |
| `cargo test --offline --no-default-features --features native,test-support --quiet -- --skip review_p2` | 293 library, 25 integration, 25 doctests pass. Log: `target/mq01-native-only.log`. |
| `cargo test --offline --no-default-features --quiet -- --skip review_p2` | 96 library, 5 integration, 25 doctests pass. Log: `target/mq01-minimal.log`. |
| `cargo clippy --offline --all-features --all-targets -- -D warnings` | Pass; `target/mq01-clippy.log`. |
| `cargo check --offline --target i686-pc-windows-msvc --all-features --all-targets` | Pass; compilation only. `target/mq01-i686-check.log`. |
| `cargo run --offline --manifest-path tools/xtask/Cargo.toml -- verify --offline` | Runner, formatting, wrapper, minimal/native compile and lint gates pass. Stops at native-only tests: 293 pass, ten known P2 reds. `target/mq01-verify.log`. Not a green V1 run. |
| All-feature `--lib review_p2` and `--test package_contents review_p` | Twelve library failures and one missing-fixture package failure remain. `target/mq01-remaining-reds.log`, `target/mq01-package-red.log`. |

Source manifest: `target/mq01-source.sha256` covers `src`, `tests`, `support`, root Cargo files,
README, and the user guide. Final manifest SHA-256:
`038118A9876C4A347396398E8BD687E99093CB22BACC560277808AD48B1F0470`.
The broad passing matrix and normal verifier preceded the five-line fallback-fixture correction;
that source manifest's hash was
`1B2598681431E8C3BD92ECE0AD4982128FF9C41FC4C24F3B57012A7B9E8DEFA6`.
Only the targeted fallback/P2 checks and formatting were rerun for that fixture correction;
the M0.1 production code did not change.
Artifacts under `target` are ignored and may need regeneration. Linux execution, x86 Windows
runtime tests, other supported-platform gates, and M1 memory/performance observations remain unrun.

## 7. Decision log

| ID / date | Decision | Reason / scope |
| --- | --- | --- |
| MD-01 · 2026-09-05 | Existing P1/P2 correctness work precedes substantial memory work. | Agreed sequence; new performance work must not obscure known failures. |
| MD-02 · 2026-09-05 | Keep light GDS adapter/configuration work in scope; hand off broader RPC changes. | Accepted boundary; maintains independently reviewable changes. |
| MD-03 · 2026-09-05 | Preserve concurrency and lifecycle; prioritize fewer copies, early bounds, and byte budgets. | Matches mostly small messages with 16–32 communicating connections. |
| MD-04 · 2026-09-05 | Candidate profile values remain provisional. | Approved direction does not establish safe universal defaults or total-RAM guarantees. |
| MD-05 · 2026-09-05 | Maintain one Markdown tracker using the useful structure of the follow-up HTML. | Stable IDs, gates, questions, evidence, and logs survive context compression without duplicating the plan. |
| MD-06 · 2026-09-05 | User accepted MQ-01: Engine-owned handshake processing with up to two workers/four queued jobs, bounded retained data, prompt cancellation, and joined shutdown. | Preserve platform verification, socket ownership, manual driving, and DLL unload safety. Running platform verification may delay shutdown. Start M0.1 with companion red tests; numeric limits remain initial values to measure. |
| MD-07 · 2026-09-05 | HTTP address fallback is serial and ends at TCP connection success. | Preserve one request/body allocation, admission, and deadlines; retrying TLS/HTTP failures could replay body-bearing operations. |
| MD-08 · 2026-09-05 | Keep one shared fuzz seed corpus under the main crate's test fixtures. | Cargo excludes nested packages regardless of parent include patterns. Relocation preserves fuzz and unit-test inputs while making packaged tests usable. Generated campaign inputs stay ignored and excluded from packages. |
| MD-09 · 2026-09-05 | Treat `nbreq-darwin` publication as a deferred release prerequisite, following the support-crate approach used for winpoll. | Online registry checks on all three bridge hosts confirm the crate is unavailable. Owner acknowledged the future crates.io process. Local overrides permit archive validation and M1 work; this session does not publish. |
| MD-10 · 2026-09-05 | Add immutable Engine-specific DER roots through the existing platform verifier. | User identified private-CA trust as a valid-use-case blocker. Supplement system trust, keep hostname/expiry/signature verification and Engine isolation, validate at construction, and avoid host-store mutation. No PEM convenience layer or custom-verifier API. E-08 proves Windows/Linux/Mac behavior; pinned Android extra-root support is unavailable. |
| MD-11 · 2026-09-05 | Correct the streaming plaintext bound for partial-record carry. | M1's valid larger HTTPS stream exposed a real failure. Focused red/green proof accounts for one carried record without widening socket input or removing backpressure; all platform gates pass. |
| MD-12 · 2026-09-05 | Use separate fixture/client processes and plain/meter observations for the baseline. | Preserve the legacy F5 observer and distinguish exact logical heap from sampled process memory, native ownership, fixture cost and allocator retention. Freeze source/lockfiles and require correctness/cleanup before accepting timings. No small-device default or performance threshold is chosen from M1 alone. |
| MD-13 · 2026-09-07 | User accepted MQ-02 Option C: track retained NBReq body ownership, share immutable response bodies, and allow explicit unique Vec transfer to application ownership. | Existing borrowed/copying consumers remain supported. Response clone semantics change from deep body copies to shared immutable bytes; consuming APIs are additive. M2 establishes ownership and removes copies; M3 implements charges/reservation after MQ-03. |
| MD-14 · 2026-09-08 | M2.4 internal retention policy: release fully consumed TLS plaintext; on reusable HTTP idle parking, trim send capacity above 128 KiB to 64 KiB. | Empty plaintext storage is replaced on the next receive anyway. Hysteresis preserves ordinary send-buffer reuse, connections, TLS sessions and valid payload ceilings. Large repeated uploads may regrow/retrim storage; this is not aggregate admission or a whole-device budget. E-11 records proof and measurements. |
| MD-15 · 2026-09-08 | User explicitly chose “Move GDS to 0.2 now; use local nbreq builds” after clearing the workspace. | Supersedes isolated-checkout recommendation. Use the existing local-build/test overrides with an explicit =0.2.0 manifest; verify x86 DLL with SkipCopy. Preserve unrelated GDS work and leave publication/deployment separate. |

| MD-16 · 2026-09-08 | User accepted MQ-03 Option C and asked for functioning knobs before choosing values. | Explicit opt-in aggregate cap; reserve existing request storage before admission, known reply length at framing and unknown replies before bounded growth. Fail with a distinct Limit reason, no waiting or hidden replay. GDS's usual roughly 1 MiB replies guide later tuning; retain valid exceptional ceilings. |
| MD-17 · 2026-09-08 | M3 implementation uses exact-size boxed storage for controlled growth and shared cleartext upload windows. | Charge old/new overlap, reserve known lengths before payload allocation, and keep permits with receive events/TLS plaintext across worker cancellation. Cleartext sends retain the original admitted allocation rather than requiring a copied payload queue. TLS session/record output storage remains separate headroom; raw receive windows are conservatively charged including framing. |
| MD-18 · 2026-09-08 | Correct the public DNS fixture's port reservation order during M3 gates. | Windows UDP selected 64 consecutive ports excluded for TCP (WSAEACCES 10013); TCP-first probe passed 64/64. Alternate TCP-first/UDP-first, matching the existing private DNS fixture strategy. No host exclusions or production DNS behavior changed. A retains failure evidence; B changes only this test helper. |
| MD-19 · 2026-09-08 | Preserve uncapped receive batching, append validated payload spans in bulk, and use vectored shared-upload writes after initial M3 observations exposed overhead. | C/D/E retain accounting and early limits. Seventeen companions include forced short writes across wrapped headers/body; E passes all gates. Longer small-message throughput is comparable; Linux latency variation and separate TLS overhead remain explicit in E-13. |
| MD-20 · 2026-09-08 | User authorizes practical GDS workload admission: keep ordinary usage controlled, slow incoming work under pressure and protect small requests. | M4.1 implements shared inbound watermarks, outgoing reservations, small-work headroom and eligible-origin poll admission. Retain 24 MiB body compatibility; simultaneous exceptional work may be refused before encoding. The initial 128 MiB outbound allowance is not preallocated or a whole-process cap. No automatic eviction/replay; full device profiles and further protocol work remain separate. E-14 records verification, low component overhead and the same-origin pickup-latency tradeoff. |

## 8. Progress log

Append concise entries. Keep the resume checkpoint and item status current as well as this history.

| Date / work | Progress | Evidence / next move |
| --- | --- | --- |
| 2026-09-05 · M0 baseline | Fourteen red regressions established; no production fixes. | E-01. Start M0.1 with bounded verification/lifecycle design. |
| 2026-09-05 · memory scope | Source analysis and memory plan accepted; broader GDS handoff prepared. | E-03, MD-01–MD-04. Keep M1–M4 unstarted until M0 completes. |
| 2026-09-05 · tracker structure | Adopted resume checkpoint, stable item IDs/status, principles, dependencies, open questions, verification/evidence, and decision/progress logs. Documentation-only change. | Links and structure checked; runtime tests not rerun. Next implementation item remains M0.1. |
| 2026-09-05 · MQ-01 investigation | Recorded proposed bounded handshake offload, cancellation/resource ownership, saturation, and joined shutdown behavior. No production/test changes; proposal is not an accepted decision. | E-04. Finalize MQ-01, then reproduce the existing red and add lifecycle/saturation companions before the M0.1 fix. |
| 2026-09-05 · MQ-01 acceptance | User accepted the proposal and authorized implementation. M0.1 started; no additional design blocker identified. | MD-06. Reproduce the red, add companion proof, implement and verify the TLS fix. |
| 2026-09-05 · M0.1 verified on Windows | Reproduced P1, added three failing lifecycle tests, implemented bounded handshake offload, and expanded to 11 companion tests. Updated shutdown documentation and preserved remaining P2 reds. | E-05. No running commands. Next: M0.2 HTTP address fallback; Linux/runtime platform gates remain explicit remainders. |
| 2026-09-05 · fallback fixture verification | Stopped a hung parallel P2 run; isolated rerun exposed inherited nonblocking accepted sockets on Windows. Corrected only the fixture and confirmed the intended fallback failure and all twelve library reds in a subsequent parallel run. | E-05. M0.2 production fix remains next; no test processes left running. |
| 2026-09-05 · M0.2–M0.9 authorization | User authorized completing the remaining M0 items and the full verifier. HTTP fallback red reproduced again at the intended Connect failure. | Implement M0.2 with serial ownership-preserving attempts, then continue through M0.9. |
| 2026-09-05 · M0.2–M0.8 targeted green | Fixed serial HTTP fallback, DNS cache caps, TCP waits/abort/finish registration, HTTP 205 framing, and DNS ID allocation. Original library reds pass individually; added focused companions. | E-06. M0.9 shared corpus relocation and archive/full-verifier checks underway. |
| 2026-09-05 · M0.9 package green | Moved all 17 seeds byte-for-byte into the shared packaged fixtures and updated fuzz commands. Final 71-file archive compiles and passes 366 unit tests with local support-crate overrides. | E-06, MD-08. Registry-only validation remains a release check; nothing published. |
| 2026-09-05 · M0 accepted on Windows | Normal verifier 24/24; all fourteen original regressions green; 23 companions added across M0. Windows x64/x86 each pass 366 library, 27 integration, 26 doctests. Tracker and historical red record updated. | E-06. No running commands; no coding blocker for M1. Next: measurement workloads and peak live-allocation observation. Linux/other-platform, registry-only packaging, and performance claims remain unverified. |
| 2026-09-05 · M0 bridge matrix complete | Same snapshot B passes all six stable/MSRV verifiers on Linux and physical Intel/Apple Silicon Macs. Fixed only the Darwin fallback fixture and proved it still rejects first-address-only behavior. Darwin helper tests/lint, extracted packages, F5 modes, and trusted HTTPS pass. | E-07, MD-09. Evidence and prepared labs retained; all test/observer processes finished. `nbreq-darwin` publication remains a release prerequisite. Next: M1 representative workloads and peak live-allocation observation. |
| 2026-09-05 · Linode cache cleanup | User authorized removing obsolete nbreq material from before 2 September. Removed 38 verified Cargo build caches; recovered 24.20 GiB. Kept old source/evidence for review and preserved all newer labs. | [Cleanup list](nbreq_linode_cleanup_20260905.md). Current M0 hashes and binaries rechecked; 27.44 GiB free. Old Git checkout has local curl edits and an untracked resolver experiment. M1 remains next. |
| 2026-09-05 · M1 accepted | Added Engine private-CA roots, allocation meter, separate fixture/controller, complete workload matrix, and a red/green fix for streaming TLS partial-record carry. Final source C satisfies current Apple fixture policy. All seven full production verifiers and 276 final observations pass; data and logs archived. | E-08, MD-10–MD-12. Windows/Linux ordinary small traffic is modest; buffered large-transfer copies and streaming windows deserve first attention. M2 next; settle MQ-02 before a public consuming/shared body API. Actual GDS and 128/256 MB device acceptance remain M4. |
| 2026-09-07 · MQ-02 investigation | Inspected waiter/callback ownership, serialization/redirect copies and GDS conversion. Proposed retaining body charges through returned/shared responses, with explicit unique Vec transfer into application ownership; compared active-only and delivery-boundary alternatives. | Section 5 proposal awaits agreement. Documentation only; no production/test changes or new runtime results. MQ-03 reservation/exhaustion remains separate. |
| 2026-09-07 · MQ-02 acceptance / M2 start | User accepted Option C after confirming existing copying usage remains supported and consumers may opt into no-copy transfer. Scoped M2 into stable sub-items; starting response delivery and body ownership (M2.1/M2.2) with failing tests. | MD-13. No budget is implemented by accepting this contract. |
| 2026-09-07 · M2.1/M2.2 verified | Shared response bodies and moved delivery implemented. Seven initial reds/two intermediate failures resolved; 13 companions, seven current-source full verifiers and 120 paired cases pass. Allocation churn improves; peak RAM does not materially fall. | E-09. Source/evidence archived and checksummed; jobs finished. Stale cached artifacts were excluded/rebuilt. ARM streaming assertion failed once, then five isolated/full unchanged rechecks passed; cause remains unproven. Next M2.3. |
| 2026-09-07 · M2.3 request ownership | Removed premature body serialization, bounded cleartext output, borrowed TLS body slices and moved redirect bodies. Six intended reds resolved; a further red caught an extra small-request TLS record, fixed with vectored header/body slices. | E-10. Existing request API/valid ceilings preserved. A MSRV fixture lint corrected without weakening assertions; superseded A/B runs retained. |
| 2026-09-07 · M2.3 validation findings | Recurring ARM shutdown assertion led to a deterministic red: reactor teardown could precede stream cancellation. Delayed stop publication fixes the proven race. A later Windows DNS fixture failure exposed inherited nonblocking accepted sockets; D normalizes fixture I/O. | E-10. C/D production inputs are identical. 30 ARM shutdown repetitions and 20 final Windows DNS repetitions pass; original failures and diagnostic limits remain recorded. |
| 2026-09-07 · M2.3 verified | Seven final D full verifiers, x86 companions, 120 C paired cases and 20 timing follow-ups pass. Roughly 100 KiB less allocation per 50 KiB request; 4 MiB phase peaks fall 14.4→10.5 MiB Windows / 14.5→12.0 MiB Linux. | E-10. Four source snapshots and 2,270 evidence files archived with checksums; all jobs/processes finished. Small-call peaks and retained capacity barely change; timing is noisy and HTTPS allocation count rises by roughly one. Next M2.4, then light M2.5; M3 budgets and actual-device acceptance remain open. |
| 2026-09-08 · M2.4 implementation / validation | Three intended capacity reds resolved; six companions cover partial bytes, clean idle parking, threshold boundaries, large-to-small connection reuse and cancellation. A full gates exposed an existing fixture that closed before idle parking. B synchronizes the intended state and keeps all eviction assertions. | E-11, MD-14. A failures retained on every host; B production is unchanged from A. Windows and both Macs pass; Windows comparison/timing complete. Finish Linux gates/measurements and archive final evidence. |
| 2026-09-08 · M2.4 verified | Seven B full verifiers, 29 x86 M2 tests, 120 paired cases and 30 longer timing cases pass. Mixed-workload post-large idle heap falls 1.88→0.94 MiB Windows / 1.95→1.01 MiB Linux; ordinary churn and small-call retention remain essentially unchanged. | E-11. Two source snapshots and 1,049 evidence files archived and checked; all jobs finished. Timing remains qualified; repeated large uploads may regrow/retrim buffers. Next M2.5 light GDS conversion; MQ-03 before M3 budgets. |
| 2026-09-08 · checkpoint and M2.5 inspection | Commit 2f89bef records review/memory work through M2.4; older F5 edits remain outside it. Read GDS's adapter, manifest/lockfile and build/test helpers. Scoped two consuming conversion sites, pointer/capacity reds, shared fallback and error-precedence companions. | M2.5 outline above. GDS still pins 0.1.0; existing local overrides require an explicit compatible version constraint. Recommend isolated 0.2 integration first. No GDS code, dependency, build or deployed DLL changed. |
| 2026-09-08 · M2.5 verified / M2 accepted | User chose main GDS 0.2 with local builds. Three allocation reds resolved; unique String conversion and explicit shared fallback preserve all response semantics. Cargo patch conflict resolved with compatible Darwin dependency range. | E-12 / MD-15. X86 HTTP/WebRPC, native/ureq DLL builds, Linux adapter and both Mac stable/MSRV gates pass. 89 private GDS evidence files and public Mac source/evidence archived. Installed DLL unchanged; all jobs finished. MQ-03 next before M3. |
| 2026-09-08 · M3 accepted | User accepted Option C and asked for working knobs before values. Implemented per-request early limits, retained-capacity accounting, bounded reservation/growth, staging ownership and GDS startup controls. Preserved actual runtime reds and subsequent fixture/harness failures; refined measured hot paths. | E-13 / MD-16–MD-19. Final E seven full gates, seventeen x86 companions, 120 memory cases, 36 timing cases, four cap probes and GDS checks pass. Five source archives, 2,696 public evidence files and 45 private GDS files checked. All jobs finished; installed DLL unchanged. M4/MQ-04/MQ-05 are next. |
| 2026-09-08 · M4.1 verified | User authorized workload admission and small-request protection. Seven runtime reds fixed; final B adds outgoing-pressure intake pauses. C corrects the component benchmark clock without changing B production. | E-14 / MD-20. X86 native/ureq suites and both builds pass; portable Windows/Linux stable/MSRV policy checks pass. 191 private files archived; all jobs finished. Component reservation overhead is low, but whole-GDS memory and same-origin pickup latency require representative installation measurements. nbreq source and installed DLL unchanged; no commit. |
| 2026-09-10 · isolated release checkpoints | Clean Darwin `4a6c807` and root `7693d6c` preserve scoped M2.5/M3/R1 work without disturbing main's F5 edits. Exact Darwin archive passes both Mac stable/MSRV tests and stable lint. Windows DNS fixture error 10054 was reproduced with a closed UDP peer; scoped fixture correction and all 24 verifier steps pass. | [Clean candidates](nbreq_020_clean_candidates.md) records package hashes, follow-up consumer evidence and pending gates. W-01 Rust panic under Wine is unresolved; inspect its reproducer and test assumptions before acceptance. No publication or GDS changes. |
| 2026-09-10 · W-01 fixed | Reproduced exact ipconfig/widestring alignment panic independently of GDS on Wine 5 x86. Clean `149450d` adds checked byte parsing in winpoll 0.1.1, removes the two production dependencies and preserves DNS policy/scopes. | [Report](nbreq_wine_dns.md), [manifest](evidence/nbreq_wine_dns_artifacts.json). Native Windows verifier 24/24, x86/x64-MSRV policy/helper checks and actual Wine default/native-only lifecycle probes pass. Original failed attempts retained. Fix mirrored to main, unrelated edits preserved. GDS installed DLL unchanged; startup/scan/shutdown acceptance and support publication remain open. |

## 9. Update and handoff discipline

After a meaningful change, test result, decision, or pause:

1. Update the current package, exact next action, test state, source identity, and obstacles in the
   resume checkpoint. Replace stale present-tense claims there; preserve history in the logs.
2. Update the affected work-item status and link evidence. Distinguish red reproduction, implemented,
   verified-on-one-target, and fully accepted. Never infer cross-platform success from a local run.
3. Append the result and next move to the progress log; resolve or supersede question/decision IDs.
   Record named remainders when a package is partially complete.
4. Before context compression or session handoff, record any running commands, uncommitted changes,
   failed checks, and the smallest next executable step. Do not rely on conversation history.
5. Keep the GDS handoff synchronized when shared API or compatibility decisions change. Preserve the
   red-test record as the original proof and attach green-fix evidence here rather than erasing it.

## 10. Later register and source anchors

Deferred unless measurement justifies them: custom allocators, general-purpose buffer pools, TLS
stack replacement, thread-stack tuning, and broader RPC protocol redesign. They are not M0 gates.

Source anchors: [defaults](../src/types.rs), [admission/completion](../src/registry.rs),
[HTTP transport](../src/backend/native_http.rs), [TCP queues](../src/tcp/io.rs),
[F5 observer](../tools/f5-observe/README.md), and [GDS details](gds_memory_handoff.md).
