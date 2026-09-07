# M1 memory baseline and next steps

2026-09-05 · NBReq 0.2.0 · M1 / E-08 · **276 valid final cases; all required M1 gates met.**

The representative small-message workload is far lighter than the configured body ceilings
suggest. The main remaining concerns are whole-body copies during large buffered operations,
full streaming queues, and the lack of an aggregate buffered-body budget. The measurements do
not establish that GDS plus its OS fits a 128 MB device; GDS integration and target-device tests
remain M4 work.

Two useful prerequisites were completed during measurement development:

* `EngineConfig::with_additional_tls_root_certificate` adds an Engine-specific DER authority
  alongside system trust. It enables private-CA deployments and verified local test fixtures.
  Malformed DER fails construction; hostname/expiry checks and Engine isolation remain intact.
  Windows/Linux/macOS are verified. The pinned Android verifier cannot add roots, so nonempty
  additional-root configuration explicitly returns Unsupported there.
* A valid longer HTTPS stream exposed an incorrect plaintext bound across TLS record fragments.
  A deterministic 512 KiB regression failed, then passed after allowing the 18 KiB wire window
  plus one carried 16 KiB plaintext record. Socket reads and backpressure remain bounded. This
  is a correctness fix, not a body-limit reduction. Logical plaintext length and Vec capacity
  remain distinct; allocation measurements capture actual requested capacity.

## Ordinary buffered traffic

Each request uploads and receives the stated payload. There are 32 actual concurrent connections
to one origin, with per-origin active/idle capacity explicitly raised from 8/4 to 32; all other
Engine defaults remain. Rows report the **largest observed peak across three fresh-process
repetitions and all ordinary phases**, including cold connections, steady traffic, retained
responses, release and shutdown. All memory values are MiB (1,048,576 bytes).

Windows private bytes are private commit; Linux RSS is resident process memory. They are useful
for their respective platforms, but are not interchangeable counters. Rust heap figures come
from separate instrumented runs, not from subtracting process counters.


| Protocol | KiB each way | Windows private | Windows working set | Linux RSS | Rust heap Win / Linux |
| --- | --- | --- | --- | --- | --- |
| HTTP | 1 | 2.03 | 9.67 | 4.92 | 0.34 / 0.65 |
| HTTP | 8 | 3.31 | 10.65 | 5.46 | 0.95 / 1.05 |
| HTTP | 50 | 6.99 | 14.29 | 10.31 | 4.57 / 5.01 |
| HTTPS | 1 | 4.46 | 14.48 | 5.87 | 0.58 / 0.65 |
| HTTPS | 8 | 6.16 | 15.93 | 6.82 | 1.55 / 1.67 |
| HTTPS | 50 | 10.03 | 19.68 | 11.61 | 5.03 / 5.65 |


At 50 KiB each way, changing concurrency gives:


| Protocol | Connections | Windows private peak | Linux RSS peak |
| --- | --- | --- | --- |
| HTTP | 1 | 2.19 | 4.73 |
| HTTP | 16 | 4.95 | 7.70 |
| HTTP | 32 | 6.99 | 10.31 |
| HTTPS | 1 | 3.99 | 6.11 |
| HTTPS | 16 | 7.24 | 8.80 |
| HTTPS | 32 | 10.03 | 11.61 |


These are whole **test-client** process observations, including caller-owned payloads, native
runtime and driver overhead. They are not an estimate of the incremental DLL cost inside GDS.
There is no evidence here that the large configured ceilings are all allocated at Engine startup.
Nor do these ordinary cases make a rare large request safe under a small total application budget.

## Where the larger peaks occur

The extended case keeps one Engine alive through slow peers, long polls mixed with bursts,
cancellation/replacement, slow streaming readers, a 4 MiB buffered upload/reply, and idle recovery.
Below are HTTPS phase maxima across three repetitions. End-of-phase live heap is the median;
peaks and live endpoints need not come from the same repetition.


| Phase | Windows private peak | Linux RSS peak | Rust heap peak Win / Linux | Live heap after Win / Linux |
| --- | --- | --- | --- | --- |
| driver_idle | 1.04 | 0.84 | 0.01 / 0.01 | 0.01 / 0.01 |
| engine_idle | 1.65 | 4.33 | 0.08 / 0.65 | 0.06 / 0.13 |
| cold_connections | 7.59 | 10.70 | 4.56 / 4.64 | 2.41 / 2.54 |
| steady | 9.86 | 11.66 | 5.22 / 5.65 | 2.42 / 2.49 |
| burst_retained | 10.29 | 11.41 | 4.85 / 5.59 | 3.99 / 4.05 |
| released_idle | 10.29 | 10.86 | 3.99 / 4.05 | 2.42 / 2.48 |
| slow_peer | 11.05 | 11.67 | 5.60 / 5.74 | 2.05 / 2.17 |
| long_polls_and_burst | 7.94 | 10.40 | 2.39 / 2.46 | 2.23 / 2.29 |
| cancel_and_replace | 9.79 | 10.91 | 3.98 / 4.04 | 0.51 / 0.58 |
| slow_reader_held | 16.08 | 16.51 | 10.67 / 10.99 | 10.14 / 10.21 |
| slow_reader_drain | 20.29 | 17.25 | 11.74 / 12.06 | 1.39 / 1.46 |
| large_transfer_retained | 15.65 | 29.10 | 14.40 / 14.47 | 5.89 / 5.96 |
| post_large_idle | 12.68 | 25.04 | 5.89 / 5.96 | 1.89 / 1.96 |
| shutdown_idle | 8.66 | 25.04 | 1.88 / 1.95 | 0.01 / 0.01 |


The slow-reader workload deliberately fills 32 default 256 KiB response windows: **8 MiB of
reserved queue capacity**, with additional TLS/decoder/driver ownership. Reservations are not
allocations in general; these 512 KiB replies are specifically chosen to reach backpressure.
Reducing windows to 16 KiB would reduce that reservation to 0.5 MiB. Actual RAM savings and
throughput effects still need an M4 comparison; this run does not change those defaults.

The 4 MiB buffered transfer exposes simultaneous ownership beyond the final reply. Source
inspection confirms a full body copied into serialization, additional serialized-buffer clones
at the TLS/plain transport boundary, and a Request retained for redirects. The aggregate meter
does not attribute each byte to a call stack, so treat the exact contribution of each copy as an
M2 experiment. A 24 MiB GDS operation deserves explicit admission/headroom even if ordinary calls
are small; do not silently reduce its valid message ceiling to make a benchmark fit.

Dropped responses and joined shutdown return measured live Rust ownership close to the driver
baseline. Process memory can remain higher because it also contains native allocations, allocator
retention, stacks and mapped pages. This measurement does not identify the retained native owner
or prove a universal absence of leaks. Avoid interpreting a flat RSS curve as retained Rust bodies.

## Performance and fixture overhead

These are **plain**, uninstrumented steady-phase results for 32 connections and 50 KiB each way.
Throughput is median [minimum–maximum] over three repetitions; p95 is the median of per-run p95s.
Latency is submit-to-waiter-collection in submission order, including fixture service and byte
validation. Repeated concurrent waves are not a constant-arrival-rate workload. The short local
runs provide a future before/after baseline, not a cross-machine ranking or a WAN prediction.
The observed ranges are substantial; small timing differences need further controlled repeats.


| Host | Protocol | Requests/s | Observed p95 ms | Client CPU ms/request |
| --- | --- | --- | --- | --- |
| Windows x64 | HTTP | 5431 [4271–5550] | 4.82 | 0.244 |
| Windows x64 | HTTPS | 4868 [3594–4882] | 5.91 | 0.275 |
| Linux x64 | HTTP | 1195 [1191–1407] | 29.84 | 0.449 |
| Linux x64 | HTTPS | 991 [973–1120] | 37.13 | 0.527 |


Allocation churn from the separate **metered** steady runs, including driver ownership and
validation, shows another useful M2 comparison. Requested bytes are cumulative successful
allocation/reallocation sizes, not simultaneously resident memory; realloc counts a full new
logical allocation size. This is not a count of bytes copied. Figures below are medians per completed request.


| Protocol | Windows requested KiB | Windows alloc/realloc calls | Linux requested KiB | Linux alloc/realloc calls |
| --- | --- | --- | --- | --- |
| HTTP | 381.1 | 57.2 | 382.3 | 55.4 |
| HTTPS | 688.3 | 83.6 | 689.2 | 82.4 |


The fixture is always an uninstrumented separate process. Its response buffers, generated private
keys, sockets and server threads are excluded from client heap/RAM/CPU. Its own observed overhead
is retained here; do not subtract these maxima from client maxima.
Fixture observations span the client's phases; fixture startup/CA-generation CPU occurs before
the first sample. The single-vCPU Linux host also shares that core with the fixture/controller,
so excluding their CPU from accounting does not remove their scheduling contention.


| Host | Fixture RSS peak ordinary HTTPS | Fixture RSS peak extended HTTPS | Fixture CPU seconds ordinary / extended |
| --- | --- | --- | --- |
| Windows x64 | 11.54 | 14.06 | 0.234 / 0.438 |
| Linux x64 | 6.51 | 7.83 | 0.160 / 0.260 |


## Additional platform checks

Windows x86 under WoW64 and the two physical Macs each ran 12 cases: three repetitions of
HTTP/1 KiB/one connection and extended HTTPS/50 KiB/32 connections, each plain and instrumented.
Their extended HTTPS maxima follow. Mac process figures use 100 ms `ps` RSS sampling, not private
bytes or physical footprint, and can miss short peaks. Windows/Linux use approximately 10 ms
sampling. Exact logical allocation peaks come from the meter in separate runs on every host.


| Host | Sampled process RSS peak | Rust heap peak | Live Rust heap after shutdown |
| --- | --- | --- | --- |
| Windows x86 | 28.38 | 14.31 | 0.01 |
| Intel Mac | 44.14 | 14.41 | 0.01 |
| ARM Mac | 52.30 | 14.41 | 0.01 |


## Verification and provenance

* Final observations: **120 Windows x64 + 120 Linux x64 + 12 Windows x86 + 12 Intel Mac +
  12 Apple Silicon Mac = 276 valid cases**. Every case checks exact upload/response bytes,
  actual fixture concurrency, normal-case connection reuse, unexpected failures, terminal
  accounting, queue release and joined shutdown. All measured children exit 0 without forced kill.
* The controller's four real-process checks pass on Windows x64, Linux and both Macs: corrupted
  responses invalidate a run; timeout cleanup joins both owned children; normal traffic proves
  exact accounting/reuse/concurrency; public system trust works with and without the private CA.
* Final production code passes the standard **24/24 verifier** on Windows x64 and all six remote
  stable/MSRV runs. The five meter tests and both tool clippy gates pass on both remote toolchains.
  Windows x86 passes the four roots tests, two streaming-TLS tests, five meter tests and release
  builds. Windows targeted checks/lints and formatting pass on final fixture C.
* The full verifiers used source B. Final source C differs **only** in the two generated-certificate
  fixtures: `tests/tls_roots.rs` and the observer's `fixture.rs`. C uses a fresh 31-day leaf with
  serverAuth EKU, satisfying [Apple's TLS certificate requirements](https://support.apple.com/en-us/103769). C root tests/tool lints pass on stable/MSRV
  on all remote hosts, with source manifests checked before/after. All production bytes are
  identical to B. No verifier bypass or test skip was introduced.
* One Windows verification attempt hit the existing DNS fixture's UDP reset (10054); the unchanged
  isolated full rerun passed. Both Mac B observer fixtures were correctly rejected for their
  certificate policy; these failed runs are not performance evidence. The logs remain included.
  The Apple expiry error remains the pinned verifier's broader CertificateInvalid classification.
* M1.1 peak tests and M1.0 roots tests have intended red/green logs. M1.5's longer stream has a
  deterministic red/green regression for the exact plaintext-bound failure. The existing 64 KiB
  streaming test remains and both assert exact bytes, bounded retention and backpressure.

Source: HEAD `b4c4d74cea0e` plus dirty M0/M1 and pre-existing work; **not clean-commit evidence**.
The legacy F5 main and README are byte-identical to their pre-M1 copies. No GDS implementation,
system trust store, commit, publication or deployed service was changed. `nbreq-darwin` publication
remains a separate release prerequisite; local support paths are used in these builds.

Final archive SHA-256: `d7707cc6bb170f3fafc64999c6642976fddb4b0c65e53b243bf12046a7d26a1f`.
Manifest SHA-256: `36886e7d54643a8aa60cae9c6f9f099c90a44d65d3dee246ae3b56334e1aa4ed`
(114 manifested files plus the manifest). Both observer modes use native + resolver; release
optimisation is Cargo's default. The tool/root lockfiles are preserved; their transitive locked
versions need not be identical to each other. Windows uses Rust 1.97.1; the three remote release
builds use Rust 1.98.0. Remote MSRV checks use Rust 1.85.0.


| Host | OS / architecture | Logical CPUs | Cases |
| --- | --- | --- | --- |
| Windows x64 | Windows-11-10.0.26200-SP0 | 8 | 120 |
| Windows x86 | Windows-11-10.0.26200-SP0 | 8 | 12 |
| Linux x64 | Linux-5.4.0-216-generic-x86_64-with-glibc2.29 | 1 | 120 |
| Intel Mac | macOS-15.7.9-x86_64-i386-64bit | 4 | 12 |
| ARM Mac | macOS-26.6.1-arm64-arm-64bit | 8 | 12 |


Host RAM: Windows approximately 31.5 GiB usable physical RAM; Linode 2 GiB class; both Macs 8 GiB.
The exact host reports, binary hashes, per-phase samples, CPU deltas, allocation churn, latency,
fixture statistics and cleanup receipts are in the evidence. Builds/tests did not run alongside
the final measurement on the same host. Other host activity was not globally controlled.

Durable artifacts: [source C archive](evidence/nbreq-m1-source-c.tar.gz),
[measurement and verification evidence](evidence/nbreq-m1-evidence-20260905.tar.gz),
[aggregate JSON](evidence/nbreq_m1_summary.json). Reproduction, phase definitions, process-counter
semantics and limits are in the [observer README](../tools/f5-observe/memory/README.md).
The [work tracker](nbreq_memory_plan.md) owns acceptance and next steps.

## M2 recommendation

1. Start with internal whole-body copies and premature serialization. Measure each change against
   the same plain/meter matrix, retaining enough request ownership for redirects and serial
   address fallback. Prove cancellation, early responses, reuse and joined release as well as bytes.
2. Resolve **MQ-02** before selecting a consuming/shared public response-body API or changing its
   budget ownership. Returning a bare Vec can move memory outside Engine accounting; that must be
   an explicit contract. Then apply compatible light GDS conversion changes through its adapter.
3. Trim demonstrably oversized retained buffers selectively; preserve useful small-buffer and
   connection reuse. Use live-capacity tests plus process measurements, rather than expecting the
   allocator to return every freed page immediately.
4. Keep M3's early per-request limits and aggregate buffered budget on the critical path. Lowering
   accepted-request count alone does not bound body bytes. MQ-03 must prevent partial bodies from
   exhausting a shared budget while all wait for more space.
5. Evaluate 16 KiB stream windows and 4–8 idle connections in M4 alongside GDS admission. Finalise
   MQ-05 using the real application and a constrained target. No new default memory ceiling or
   performance threshold is accepted by this baseline; 128/256 MB whole-device operation remains
   a target to prove, with OS/kernel/socket/application headroom included.
