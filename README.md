# NBReq

NBReq is a Rust HTTP client for programs that need concurrent network access, prompt cancellation,
deterministic shutdown, and synchronous or callback-oriented APIs without adopting an async
runtime.

## Highlights

- Simple blocking HTTP requests for ordinary use, with callbacks, direct waiters, streaming
  uploads, and streaming responses for advanced scenarios.
- Prompt cancellation across DNS, connection, TLS, upload, and download work—shutdown does not
  wait for slow network timeouts.
- Run networking on an owned background thread, or drive it manually from a single thread.
- No Tokio or other async runtime required.
- Bounded queues, resource limits, connection pooling, structured errors, and deterministic joined
  shutdown.
- Rust-native HTTP/1.1 and TLS, supporting Windows and Linux.

## Quick start

The default build is self-contained Rust HTTP/1.1. Create one independently owned Engine and use
its GET/POST convenience methods for ordinary buffered requests:

```rust
use std::time::Duration;

use nbreq::{Engine, EngineConfig};

let engine = Engine::new(EngineConfig::spawned())?;
let response = engine
    .get("https://example.com/")
    .connect_timeout(Duration::from_secs(5))
    .total_timeout(Duration::from_secs(15))
    .call()?;

println!("status {}, {} bytes", response.status(), response.body().len());
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

HTTP error status codes are responses. DNS, connection, TLS, timeout, limit, cancellation, and
shutdown outcomes remain distinct. The convenience builder uses the same Engine, connection pool,
limits, and request path as an explicit Client; it installs no hidden global runtime. Use
`Engine::client()` for callbacks, direct waiters, prompt cancellation, manual driving, or streaming.

See the [consumer guide](docs/getting-started.md) for callbacks, direct waiters, manual driving,
streaming uploads/responses, cancellation, GUI/FFI ownership, and shutdown.

The default feature set includes both the native network stack and the public `Resolver` API.
HTTP-only consumers can omit the public Resolver and Windows search-suffix registry reader while
retaining native HTTP plus exact-name DNS for HTTP and hostname `TcpConnector`:

```toml
[dependencies]
nbreq = { version = "0.1", default-features = false, features = ["native"] }
```

The `resolver` feature implies `native`; disabling it never selects a blocking OS resolver or a
second network owner.

Security issues should be reported privately as described in [SECURITY.md](SECURITY.md), not in a
public issue.

## Historical curl reference

NBReq's curl Multi pilot proved the public lifecycle and supplied differential transport evidence
while the native implementation was built. It is deliberately absent from the 0.1.0 public crate:
the pilot requires a locally patched binding, while maintaining a published fork would create a
permanent support obligation for a reference backend. The accepted source, tests, scripts, and GDS
artifacts remain recoverable from project history. Native NBReq is the supported crate backend.

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

The default features build NBReq's nonblocking HTTP/1.1 stack and public Resolver using `mio` for
portable OS readiness and notification, `httparse` for response-head parsing, a bounded
Engine-owned DNS wire codec and resolver, and rustls for owner-driven TLS. None is an executor and
NBReq adopts no async runtime. The backend owns bounded socket and stream queues, all timeout
clocks, cancellation, joined shutdown, conservative pooling, redirects, and direct
`ResponseReader` delivery. Windows
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

## Project

NBReq is developed by [Cave Rock Software Limited](https://www.caverock.com/).

## License

NBReq is licensed under either of the following, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)); or
- MIT License ([LICENSE-MIT](LICENSE-MIT)).

The generated [component and dependency license report](THIRD_PARTY_LICENSES.html) records the
locked Windows and Linux release graph.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

Copyright (c) 2026 Cave Rock Software Limited.
