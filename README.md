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

## Native reactor status

The private `native` feature now builds the HTTP-independent nonblocking TCP foundation using
`mio` for portable OS readiness and notification. `mio` is not an executor and NBReq does not
adopt Tokio or any async runtime. NBReq owns socket state, generation-safe identities, bounded byte
queues, deadlines, cancellation, bidirectional half-close, a bounded wake-failure safety poll, and
shutdown. Windows and Ubuntu 20.04 raw-transfer and Engine-lifecycle fixtures pass, completing
WP6. Enabling `native` does not yet make
`Engine::new` select it—HTTP serialization and framing begin in WP7.

## Project documents

- [Initial product specification](thoughts/nbreq_initial_spec.md)
- [Delivery plan](thoughts/project_nbreq_plan.html)
- [WP2 curl pilot evidence](thoughts/wp2_curl_pilot_evidence.md)
- [WP4 adversarial HTTP laboratory evidence](thoughts/wp4_http_lab_evidence.md)
- [WP6 native reactor evidence](thoughts/wp6_native_reactor_evidence.md)
- [GDS curl-pilot integration plan](thoughts/gds_curl_pilot_integration_plan.md)
- [GDS G4 packaging and loader evidence](thoughts/gds_curl_pilot_g4_evidence.md)
- [DPWebRPC plan sample](thoughts/project_dpwebrpc_sample.html)

Without a transport feature, the ordinary constructor retains the deterministic non-networking
scaffold used to test the public lifecycle contract. `test-support` exposes deterministic controls
for downstream conformance tests; it is not needed by curl-pilot consumers.
