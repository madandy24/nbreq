# P1/P2 review regressions: red phase

Historical red-phase evidence, recorded on Windows with Rust 1.97.1 on 2026-09-05.
Subsequent fixes and green evidence are tracked in [the memory work tracker](nbreq_memory_plan.md).

Fourteen ordinary failing tests cover the nine review findings: thirteen library tests and one
packaging integration test. Thirteen tests are new; the TLS test replaces the previous test that
asserted blocking behavior. None are ignored or marked `should_panic`.

Production behavior was unchanged in this original red phase. The blocking-send test uses a `cfg(test)` observer to count
refused send attempts without measuring process CPU consumption. That observer and its field
are absent from non-test builds.

## Findings and observed failures

All filters below match ordinary test names. Prefix every filter with `review_`.

| Finding | Test filter | Desired behavior and observed failure |
| --- | --- | --- |
| P1: certificate verification blocks the reactor | `p1_certificate_verification_` | Unrelated loopback HTTP completes while certificate verification is gated. Currently it completes only after verification is released. Both requests succeed after release, and fixtures join before the failing assertion. |
| P2: HTTP discards alternate DNS addresses | `p2_http_tries_the_next_dns_address_` | After the first address refuses TCP, HTTP reaches the healthy second address. Currently it returns a transport error at `Connect`. A literal-address request proves the healthy HTTP fixture works. |
| P2: caller result limit truncates the shared DNS cache | `p2_dns_cache_preserves_` | A one-result lookup followed by an eight-result lookup retains all eight cached addresses. Currently the second caller receives one; an uncached control receives eight. |
| P2: blocking TCP send spins with insufficient partial capacity | `p2_tcp_blocking_send_waits_` | With four of eight send bytes occupied, a six-byte blocking send waits for capacity. Currently it retries sixteen times without any queue progress or notification. Cancellation wakes and joins the sender before assertion. |
| P2: abort releases budget but retains queued memory | `p2_tcp_cancel_frees_`, `p2_tcp_reset_frees_`, `p2_tcp_failure_frees_`, `p2_tcp_engine_stop_frees_` | Each abort variant frees send, pump, and unread receive allocations while handles remain alive. Currently all four retain twelve bytes of payload allocation after releasing the reservation. Queue counters must also return to zero. |
| P2: finish callback can register again after delivery | `p2_tcp_finish_callback_registration_` | A second registration returns `InvalidRequest` after the first callback is delivered. Currently it returns success. The test also requires the rejected callback never to run. |
| P2: HTTP 205 completes before chunk framing ends | `p2_205_` | Three tests cover the exact buffered boundary, fragmented final-chunk input, and streaming completion. Currently the parser consumes 58 of 63 response bytes, publishes buffered completion before the final chunk, and prematurely completes the stream. |
| P2: DNS transaction IDs increment predictably | `p2_dns_wire_ids_` | Thirty-two sequential, uncached, IPv4 lookups must not expose an entirely predictable wrapping `+1` ID sequence. Currently all thirty-two IDs follow that sequence. |
| P2: package omits compile-time fixtures | `p2_package_contains_` | Every literal `include_bytes!` dependency in the packaged DNS/HTTP fixture consumers appears in Cargo's archive inventory. Currently seventeen seed files are missing. |

## Reproduce the red phase

Run these separately: Cargo stops after a failing library test binary and would otherwise never
reach the packaging integration test.

```sh
cargo test --offline --all-features --lib review_p -- --test-threads=1
cargo test --offline --all-features --test package_contents review_p -- --nocapture
```

Expected results: thirteen library failures and one packaging failure, with the assertions in
the table above. To select one finding, replace `review_p` with its full filter, for example
`review_p2_dns_cache_preserves_`.

## Verification performed

| Command | Result |
| --- | --- |
| `cargo test --offline --all-features --lib review_p -- --test-threads=1` | All thirteen regressions fail at the intended assertions. |
| `cargo test --offline --all-features --test package_contents review_p -- --nocapture` | Fails with the seventeen omitted fixture paths. |
| `cargo test --offline --all-features --lib` | With normal parallel execution: 330 existing tests pass; precisely the thirteen regressions fail. |
| `cargo test --offline --all-features -- --skip review_p` | 330 library tests, 16 adversarial integration tests, 10 public-contract tests, and 26 doctests pass. The packaging regression is excluded by the same filter. |
| `cargo test --offline --no-default-features --features native --lib review_p -- --test-threads=1` | All eleven applicable regressions fail as intended; the two public-resolver tests are feature-gated out. |
| `cargo clippy --offline --all-features --all-targets -- -D warnings` | Passes. |
| `cargo clippy --offline --no-default-features --all-targets -- -D warnings` | Passes. |
| `cargo fmt --all --check` and `git diff --check` | Pass. |

The skip command is only a diagnostic control for this red phase. Normal test runs continue to
fail until the findings are fixed; no CI filters or verifier exemptions were added.

## Scope and fixture choices

- The TLS regression proves unrelated HTTP must progress during verification. It does not yet
  prove bounded cancellation or joined shutdown while a platform verifier remains blocked.
  Those lifecycle guarantees need explicit coverage when the verification design is changed.
- The HTTP fallback fixture reserves a non-listening loopback endpoint and checks that it
  refuses connections. Windows needs roughly two seconds to report that refusal, so the probe
  and request allow five seconds for connection establishment. The HTTP request has an
  eight-second total bound. No external server or DNS service is involved.
- The TCP memory tests inspect retained inner-buffer capacities, not just lengths, so emptying
  buffers while retaining their allocations cannot satisfy the memory-release assertion.
  They exercise the shared abort handler with each terminal reason.
- The DNS ID test rejects the specific known sequential pattern. It permits individual adjacent
  IDs, uses wrapping arithmetic, and is not a cryptographic randomness or spoofing-resistance test.
- The packaging test runs `cargo package --list --allow-dirty --offline`, without publishing or
  changing the manifest. It checks the archive inventory rather than building an extracted crate.
  Its small source scanner supports the literal `include_bytes!` calls used by the fixtures today.
- Runtime checks in this pass were Windows-only. Other operating systems were not executed.

## Memory follow-up and fix order

The [memory plan](nbreq_memory_plan.md) follows this red-test work; its scope includes light GDS
adapter/configuration changes and a [separate GDS handoff](gds_memory_handoff.md) for broader work.
Keep the current fourteen regressions. New memory budgets and default tuning are not prerequisites
for making them green.

During the fixes, add focused companion coverage where needed:

- TCP abort must free payload allocations before replacement admission, with old handles retained
  and repeated abort releasing the reservation once. Preserve graceful FIN's unread data.
- Blocking sends must sleep with insufficient partial capacity, then resume correctly on capacity
  or cancellation. Smaller future windows make this existing regression more valuable.
- A TLS offload design must bound workers, queued work, and retained data. Cover saturation and
  queued cancellation as well as joined shutdown after the verifier gate is released; the current
  red test alone does not prove these properties or interruptibility of a platform verifier.
- DNS caching must retain answers independently of caller output limits while preserving internal
  safety bounds. HTTP address fallback must retain one request's admission/accounting across attempts.

Allocation-copy tests, early per-request body-limit tests, and aggregate-memory exhaustion tests
belong to the subsequent memory phase. Documentation added here does not change test outcomes.
