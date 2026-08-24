# NBReq

NBReq is a Rust HTTP client for programs that need concurrent network access, prompt cancellation,
deterministic shutdown, and synchronous or callback-oriented APIs without adopting an async
runtime.

The architecture contract and backend-independent lifecycle kernel are complete. The default build
and ordinary constructor use NBReq's Rust-native HTTP implementation. The earlier curl Multi pilot
remains in project history as reference and differential evidence; it is not in the public crate.

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

Security issues should be reported privately as described in [SECURITY.md](SECURITY.md), not in a
public issue.

## Historical curl reference

NBReq's curl Multi pilot proved the public lifecycle and supplied differential transport evidence
while the native implementation was built. It is deliberately absent from the first public crate:
the pilot requires a locally patched binding, while maintaining a published fork would create a
permanent support obligation for a reference backend. The accepted source, tests, scripts, and GDS
artifacts remain recoverable from project history. Native NBReq is the supported crate backend;
GDS retains ureq as its deployment rollback.

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
connection measurements from internal scaffold snapshots, where the connection fields remain zero.
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
proper forbids unsafe code; the minimal WinSock FFI is isolated in the implementation-detail
`nbreq-winpoll` crate behind a safe API. `Engine::new` and an unqualified builder select native in
ordinary builds.

Explicit selection remains available when a caller wants to state the choice:

```rust
use nbreq::{Engine, HttpBackend};

let engine = Engine::builder()
    .http_backend(HttpBackend::Native)
    .build()?;
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`HttpBackend::Native` remains present under every feature combination. A
`--no-default-features` build therefore still compiles its portable configuration surface, but
network construction returns `Unsupported`; the lifecycle scaffold is internal test support rather
than a second public runtime.

## Documentation

- [Consumer guide](docs/getting-started.md)

`test-support` exposes deterministic controls for downstream conformance tests; it is not needed
by ordinary consumers.

## License

NBReq is licensed under either of the following, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)); or
- MIT License ([LICENSE-MIT](LICENSE-MIT)).

Copyright (c) 2026 Cave Rock Software Limited.
