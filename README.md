# NBReq

NBReq is a Rust HTTP client for programs that need concurrent network access, prompt cancellation,
deterministic shutdown, and synchronous or callback-oriented APIs without adopting an async
runtime.

The architecture contract and backend-independent lifecycle kernel are complete. The feature-gated
curl Multi pilot now provides the first consumer-usable spawned backend while the Rust-native
replacement is developed behind the same Engine/Client contract.

## Curl pilot use

Enable the `curl-pilot` feature, construct one independently owned Engine, and issue cheap cloneable
Clients from it. The feature changes only the private backend selected by `Engine::new`; no curl type
enters application code.

```rust
use std::time::Duration;

use nbreq::{Completion, Engine, EngineConfig, Request};

let engine = Engine::new(EngineConfig::spawned())?;
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

The private `native` feature builds NBReq's nonblocking HTTP/1.1 stack using `mio` for portable OS
readiness and notification, `httparse` for response-head parsing, Hickory's wire types for an
Engine-owned DNS service, and rustls for owner-driven TLS. None is an executor and NBReq adopts no
async runtime. The backend owns bounded socket and stream queues, all timeout clocks, cancellation,
joined shutdown, conservative pooling, redirects, and direct `ResponseReader` delivery. Windows
and exact-source Ubuntu 20.04 prove the accepted buffered and streaming paths, including bounded
fixed/chunked `UploadBody` pumping. Enabling `native` does not make `Engine::new` select it.

WP10's explicit selection seam is available without changing that current default:

```rust
use nbreq::{Engine, HttpBackend};

let engine = Engine::builder()
    .http_backend(HttpBackend::Native)
    .build()?;
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`HttpBackend` has the same public variants under every feature combination. Selecting an
implementation that was not compiled returns `Unsupported` at construction. At the separately
reviewed default-switch gate, the `native` feature will become a Cargo default so ordinary
`cargo add nbreq` consumers receive the native implementation without extra feature work; curl
will remain an explicit reference/compatibility selection.

## Project documents

- [Initial product specification](thoughts/nbreq_initial_spec.md)
- [Delivery plan](thoughts/project_nbreq_plan.html)
- [WP2 curl pilot evidence](thoughts/wp2_curl_pilot_evidence.md)
- [WP4 adversarial HTTP laboratory evidence](thoughts/wp4_http_lab_evidence.md)
- [WP6 native reactor evidence](thoughts/wp6_native_reactor_evidence.md)
- [WP7 native HTTP/1.1 evidence](thoughts/wp7_native_http_evidence.md)
- [GDS curl-pilot integration plan](thoughts/gds_curl_pilot_integration_plan.md)
- [GDS G4 packaging and loader evidence](thoughts/gds_curl_pilot_g4_evidence.md)
- [DPWebRPC plan sample](thoughts/project_dpwebrpc_sample.html)

Without a transport feature, the ordinary constructor retains the deterministic non-networking
scaffold used to test the public lifecycle contract. `test-support` exposes deterministic controls
for downstream conformance tests; it is not needed by curl-pilot consumers.
