# M3 — buffered HTTP memory controls

2026-09-08. MQ-03 Option C accepted; final numerical GDS settings remain a separate decision.
The [memory tracker](nbreq_memory_plan.md) owns the current acceptance state.

## Delivered controls

| Control | Scope and behavior |
| --- | --- |
| Request/response body limits | Optional per-request byte ceilings on all three request builders. The effective limit is the smaller of the request value and Engine ceiling. Missing inherits; zero permits an empty body. |
| `max_buffered_body_bytes` | Optional aggregate capacity cap per Engine. Default `None` preserves admission compatibility. Includes retained uploads, replies, spare capacity, and buffered receive/plaintext staging. |
| `reserved_buffered_body_bytes()` | Current and high-water reservations through resource metrics. Includes permission reserved for known bodies before their payload arrives. |
| GDS startup controls | `GDS_NBREQ_MAX_REQUEST_BODY_BYTES`, `GDS_NBREQ_MAX_RESPONSE_BODY_BYTES`, and `GDS_NBREQ_MAX_BUFFERED_BODY_BYTES`. Strict unsigned decimal bytes, read at native Engine creation; malformed values fail initialization. Direct Rust configuration is also available. |

GDS's existing request-specific response limit now reaches nbreq before receipt, while the
conversion guard remains. Engine request/response defaults remain 24 MiB in GDS and 16 MiB in
nbreq. The aggregate cap remains opt-in. The user's usual roughly 1 MiB GDS response target is
tuning guidance, not a replacement for currently valid exceptional-operation ceilings.

## Ownership and failure contract

An admitted upload adopts its existing Vec and reserves its capacity before acceptance. A
known response length reserves permission at validated framing, without allocating until data
arrives. Unknown-length bodies grow in bounded steps; replacement capacity is reserved while
the original allocation is still charged. Refusal leaves the original bytes intact.

Buffered socket events and extracted TLS plaintext carry their reservations with their actual
owners, including queued/executing TLS work during cancellation. Cleartext upload windows share
the admitted allocation. Vectored writes keep headers and body together without constructing a
combined body buffer; a forced short-write test covers wrapped headers and the body boundary.

Shared returned bodies are charged once until the last owner drops, including after Engine
shutdown. Unique consuming transfer into a Vec or GDS String releases the nbreq reservation and
passes memory responsibility to the application. The accounting state can outlive the Engine
without retaining sockets, workers, or the registry.

Exhaustion returns `ErrorKind::Limit` with `LimitKind::BufferedBodyBytes`, distinct from an
oversized individual request or reply. It does not wait for other partial bodies or silently
replay an operation. A POST may already have taken effect when its response encounters pressure;
the caller must retain that distinction when deciding recovery.

This is not a process-memory cap. Allocator bookkeeping/rounding, metadata, headers outside
body-bearing staging, stacks, sockets, TLS session/record output storage, and application-owned
copies/decoded data need headroom. Streamed response queues retain their separate existing
budget; a buffered upload is still charged when its response is streamed.

## Verification and measurements

Final E source and evidence identities are recorded in
[the artifact manifest](evidence/nbreq_m3_artifacts.json). All seven full verifiers pass:
Windows plus stable and MSRV 1.85 on Linux, Intel Mac, and Apple Silicon Mac. Each verifier
completes all 24 steps without skip filters. Final all-feature library counts are 414 on Windows,
412 on Linux, and 410 on each Mac; the platform-specific differences are expected.

The initial runtime reds proved missing per-request enforcement, missing aggregate upload and
known-length admission, and missing GDS pass-through. A companion caught known-length storage
incorrectly growing to the generic 4 KiB minimum while retaining the original reservation. All
were fixed before final verification. Seventeen focused nbreq companions cover capacity versus
length, exact lifetime, growth overlap, concurrent admission, no hidden POST retry, subsequent
progress, streaming separation, HEAD, shared sends, and cancellation of TLS worker input.

Windows final E also passes all seventeen companions on x86.
GDS x86 native HTTP 16 and WebRPC 74 tests pass against E, and the native DLL builds with
`-SkipCopy`. Ureq-only HTTP 2 and WebRPC 73 tests and its DLL build also pass. The installed DLL
is unchanged. Private GDS source and build logs stay in the GDS repository.

The final paired memory comparison uses the frozen M2.4 B runtime and final E binaries on each
host: 32 connections, HTTP/HTTPS with 1 KiB and 50 KiB bodies, plus a mixed HTTPS workload, three
alternating repetitions, plain and allocation-meter builds. This produces 60 cases per host.
Separate fixture processes and exact byte/completion/cleanup checks exclude invalid observations.
Longer small-message timing compares before, uncapped E, and E with an 8 MiB cap.

All 120 final paired cases, 36 longer timing cases, and four configured-cap probes pass their
byte, completion, and joined-cleanup checks. The table shows same-host medians, before → E,
for the ordinary 32-connection, 50 KiB workloads. Heap is measured logical live allocation,
including the observer client's other heap data; it is not RSS or the aggregate ledger alone.

| Host / transport | Heap while replies retained (MiB) | Heap after release (MiB) | Steady 512-request wall time (ms) |
| --- | --- | --- | --- |
| Windows HTTP | 3.811 → 1.815 | 1.805 → 0.245 | 113.3 → 68.1 |
| Windows HTTPS | 4.372 → 3.955 | 2.366 → 2.385 | 135.4 → 99.1 |
| Linux HTTP | 3.870 → 1.876 | 1.864 → 0.306 | 365.6 → 244.5 |
| Linux HTTPS | 4.482 → 4.051 | 2.476 → 2.481 | 439.4 → 374.8 |

Cleartext upload sharing removes retained send-buffer copies. Known-length response allocation
also reduces spare capacity. HTTPS retains separate TLS output/session storage: ordinary idle
heap is essentially unchanged. Steady HTTPS peak heap is 5.185 → 5.013 MiB on Windows and
5.638 → 5.646 MiB on Linux; this does not establish a universal peak reduction. Ordinary 1 KiB
HTTPS idle heap rises by approximately 5–7 KiB across 32 connections.

The short 1 KiB Windows HTTPS run suggested 19.4 → 24.2 ms for 512 requests. Longer checks use
256 rounds (8,192 requests), three alternating repetitions, and plain binaries:

| Host / transport | Before wall time (ms) | E uncapped (ms) | E with 8 MiB cap (ms) |
| --- | --- | --- | --- |
| Windows HTTP | 358.5 | 345.1 | 343.2 |
| Windows HTTPS | 406.5 | 402.4 | 381.8 |
| Linux HTTP | 569.1 | 526.7 | 476.6 |
| Linux HTTPS | 616.5 | 617.9 | 589.9 |

The larger run does not reproduce a material small-message throughput penalty. It does not
prove zero latency cost: Linux uncapped HTTPS p95 is 3.39 → 4.23 ms (3.22 ms capped), and HTTP
is 3.05 → 3.11 ms (2.74 ms capped). Windows p95 improves slightly in these longer observations.
CPU and sampled process-memory readings remain available in raw results; short phases and host
noise prevent precise efficiency claims. These loopback fixtures do not establish performance
or RAM acceptance for GDS on a 128 MiB device, nor should Windows/Linux host speeds be attributed
to the OS or library implementation.

Configured-cap Windows probes pass with 8 MiB for ordinary traffic and 12 MiB for the extended
fixture. The original 8 MiB extended probe correctly refused a 4 MiB upload plus a 4 MiB reply:
those two bodies leave no space for staging. That failed fixture run is preserved and is not
performance evidence. These values are experimental fixtures, not recommended GDS defaults.

## Development evidence and limits

Frozen A preserves the initial accounting implementation. B changes only the public DNS test
port-reservation helper after Windows selected UDP ports excluded for TCP: 64 failed UDP-first
attempts and 64 successful TCP-first probes established the cause. Alternating reservation
order follows the existing private DNS fixture; runtime DNS and host configuration are unchanged.

Early measurements exposed overhead. C retains existing receive batching when the aggregate cap
is disabled; enabled caps yield between receive windows so decoding can release staging. D
appends validated payload spans in bulk while retaining the framing state machine. E uses
vectored header/body writes. Earlier snapshots and measurements remain available; their results
are not substituted for final E results. An initial longer-timing command requested 512 rounds,
beyond the observer's established 256-round maximum; its rejected startup is retained separately
from the corrected timing observations.
The Linux timing launcher also wrote `0n` instead of a newline-terminated `0` in its exit marker;
the first collector refused that marker. The marker and diagnostic are preserved. Acceptance
uses the successful 18-case completion log and all individual validity/exit/join checks, not
an assumed successful marker. No production code or measurement was rerun to hide the typo.

No final small-device memory profile, whole-GDS acceptance, deployment, or registry publication
is claimed. M4/MQ-04 must settle admission across system contexts, exceptional large-operation
policy, application-owned/decoded data, and measurements on representative 128–256 MiB devices.
The [GDS handoff](gds_memory_handoff.md) retains broader consumer work.
