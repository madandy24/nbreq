# NBReq

NBReq is a Rust HTTP client for programs that need concurrent network access, prompt cancellation,
deterministic shutdown, and synchronous or callback-oriented APIs without adopting an async
runtime.

The architecture contract and backend-independent lifecycle kernel are complete. The default build
and ordinary constructor use NBReq's Rust-native HTTP implementation. The feature-gated curl Multi
pilot remains an explicitly selected reference/rollback backend.

## Quick start

The default build is self-contained Rust HTTP/1.1. Create one independently owned Engine, issue
cheap cloneable Clients from it, and consume the Engine when the service stops:

```rust
use std::time::Duration;

use nbreq::{Engine, EngineConfig, Request};

let engine = Engine::new(EngineConfig::spawned())?;
let client = engine.client();

let response = client.execute(
    Request::get("https://example.com/")
        .connect_timeout(Duration::from_secs(5))
        .total_timeout(Duration::from_secs(15))
        .build()?,
)?;

println!("status {}, {} bytes", response.status(), response.body().len());
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

HTTP error status codes are responses. DNS, connection, TLS, timeout, limit, cancellation, and
shutdown outcomes remain distinct. Clients do not own or extend Engine lifetime, and no hidden
runtime is installed globally.

See the [consumer guide](docs/getting-started.md) for callbacks, direct waiters, manual driving,
streaming uploads/responses, cancellation, GUI/FFI ownership, and shutdown.

## Curl pilot use

Enable the `curl-pilot` feature, select `HttpBackend::Curl` on the builder, construct one
independently owned Engine, and issue cheap cloneable Clients from it. Enabling the feature only
compiles the backend; Cargo feature unification never changes what `Engine::new` selects, and no
curl implementation type enters application code.

```rust
use std::time::Duration;

use nbreq::{Completion, Engine, HttpBackend, Request};

let engine = Engine::builder()
    .http_backend(HttpBackend::Curl)
    .build()?;
let client = engine.client();

let callback_handle = client.start(
    Request::get("https://example.com/")
        .total_timeout(Duration::from_secs(10))
        .build()?,
    |completion| match completion {
        Completion::Completed(response) => println!("status {}", response.status()),
        Completion::Failed(error) => eprintln!("request failed: {error}"),
        Completion::Cancelled => eprintln!("request cancelled"),
    },
)?;

let response = client.execute(
    Request::get("https://example.com/")
        .connect_timeout(Duration::from_secs(5))
        .build()?,
)?;

// HTTP 4xx/5xx are responses. Transport/policy failures are errors.
println!("{} bytes", response.body().len());
callback_handle.cancel()?; // harmless if the callback request already completed
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The curl pilot is spawned-only. Pilot deployments should configure finite connect/total deadlines;
the backend deliberately makes no prompt connect/DNS teardown claim, and the native replacement
retains that stronger proof obligation.
Curl-backed modules and the pinned curl DLL remain loaded until process exit.
Responses are buffered; HTTP error status codes remain responses, and portable trailer exposure is
not yet defined. Cancellation stops local work but cannot undo a request already acted upon by the
remote server.

Portable request header values remain byte strings. The curl pilot's Rust binding can currently
submit only UTF-8 header values; selecting that backend completes an otherwise valid opaque value
as `Failed(Unsupported)`. The native backend must not inherit this pilot-only narrowing.

Engine configuration independently bounds accepted/inflight requests, queued commands, and
callback-bearing requests/events. A terminal callback retains both its inflight and callback permit
until it returns; blocking-only requests do not consume callback capacity.

`Engine::metrics()` returns an owner-observed, nonblocking snapshot of saturating request and
connection counters plus current and high-water bounded-resource pressure. It contains no URL,
origin, header, body, address, certificate, or backend-native error data; fields may be slightly
cross-field inconsistent while work is moving. Native connection counters describe capacity
lifecycles beginning at DNS/connect reservation, matching the active cap rather than claiming that
every reserved slot completed a TCP handshake.
`EngineMetrics::connection_metrics_available()` distinguishes those native-owned physical
connection measurements from curl/scaffold snapshots, where the connection fields remain zero
rather than pretending curl exposed events that it does not.
When a buffered waiter or streaming reader observes a terminal result, its matching outcome counter
has already advanced; this ordering is part of the canonical terminal commit rather than eventual
reactor bookkeeping.

## Native backend status

The default `native` feature builds NBReq's nonblocking HTTP/1.1 stack using `mio` for portable OS
readiness and notification, `httparse` for response-head parsing, Hickory's wire types for an
Engine-owned DNS service, and rustls for owner-driven TLS. None is an executor and NBReq adopts no
async runtime. The backend owns bounded socket and stream queues, all timeout clocks, cancellation,
joined shutdown, conservative pooling, redirects, and direct `ResponseReader` delivery. Windows
and exact-source Ubuntu 20.04 prove the accepted buffered and streaming paths, including bounded
fixed/chunked `UploadBody` pumping. The Windows build also passes live GDS traffic and shutdown on
Ubuntu 20.04's stock Wine 5. Native Windows keeps Mio; only a first-registration missing-AFD error
on old Wine selects a documented `WSAPoll` compatibility path with a 50 ms safety bound. NBReq
proper forbids unsafe code; the minimal WinSock FFI is isolated in a private unpublished workspace
crate behind a safe API. `Engine::new` and an unqualified builder select native in ordinary builds.

Explicit selection remains available when a caller wants to state the choice:

```rust
use nbreq::{Engine, HttpBackend};

let engine = Engine::builder()
    .http_backend(HttpBackend::Native)
    .build()?;
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`HttpBackend` has the same public variants under every feature combination. Selecting an
implementation that was not compiled returns `Unsupported` at construction. A
`--no-default-features` build therefore still compiles, but ordinary construction returns
`Unsupported`; the lifecycle scaffold is internal test support rather than a third public runtime.

## Project documents

- [Consumer guide](docs/getting-started.md)
- [WP11 release-readiness ledger](thoughts/wp11_release_readiness.md)
- [Initial product specification](thoughts/nbreq_initial_spec.md)
- [Delivery plan](thoughts/project_nbreq_plan.html)
- [WP2 curl pilot evidence](thoughts/wp2_curl_pilot_evidence.md)
- [WP4 adversarial HTTP laboratory evidence](thoughts/wp4_http_lab_evidence.md)
- [WP6 native reactor evidence](thoughts/wp6_native_reactor_evidence.md)
- [WP7 native HTTP/1.1 evidence](thoughts/wp7_native_http_evidence.md)
- [GDS native P10-06 Windows/Wine evidence](thoughts/gds_native_p10_06_evidence.md)
- [GDS curl-pilot integration plan](thoughts/gds_curl_pilot_integration_plan.md)
- [GDS G4 packaging and loader evidence](thoughts/gds_curl_pilot_g4_evidence.md)
- [DPWebRPC plan sample](thoughts/project_dpwebrpc_sample.html)

`test-support` exposes deterministic controls for downstream conformance tests; it is not needed
by ordinary native or explicit curl consumers.

## License

NBReq is licensed under either of the following, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)); or
- MIT License ([LICENSE-MIT](LICENSE-MIT)).

Copyright (c) 2026 Cave Rock Software Limited.
