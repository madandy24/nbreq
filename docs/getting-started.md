# Using NBReq

NBReq is built around one explicit owner. An `Engine` owns network state, pools, resolver work,
callback workers, limits, and shutdown. It issues cheap cloneable `Client` command handles, but a
Client neither owns nor extends the Engine's lifetime. Keep the Engine in the service or module that
is responsible for stopping HTTP.

The default Cargo feature is `native`. It provides NBReq's Rust-native HTTP/1.1, DNS, TCP, and
rustls implementation without Tokio or another async runtime. The optional curl implementation is
an explicitly selected diagnostic/reference backend; merely compiling it never changes ordinary
construction.

Until the first public package is cut, consumers use a workspace/path dependency:

```toml
[dependencies]
nbreq = { path = "../nbreq" }
```

## Spawned mode and blocking requests

Spawned mode owns its reactor thread and, by default, one callback worker. It is the normal choice
for services, command-line programs, DLLs, and applications that want blocking convenience calls
without adopting an executor.

```rust,no_run
use std::time::Duration;

use nbreq::{Engine, EngineConfig, Request};

let engine = Engine::new(EngineConfig::spawned())?;
let client = engine.client();

let request = Request::get("https://example.com/")
    .connect_timeout(Duration::from_secs(5))
    .inactivity_timeout(Duration::from_secs(10))
    .total_timeout(Duration::from_secs(30))
    .build()?;
let response = client.execute(request)?;

println!("HTTP {}", response.status());
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`execute` returns a `Response` only for completed HTTP exchanges. A 404 or 500 remains a Response;
transport, timeout, policy, and cancellation outcomes are errors. Total timeout begins when NBReq
accepts the request, so queue time is included. TLS certificate and hostname verification is on by
default. Disable it only through the deliberately explicit `TlsVerification` compatibility option.

## Callbacks and direct waiters

`Client::start` queues one `FnOnce` callback after the canonical terminal result is committed. User
code never runs on the network reactor or while the request registry is locked. In spawned mode the
callback runs on the Engine-owned callback pool:

```rust,no_run
use nbreq::{Completion, Engine, EngineConfig, Request};

let engine = Engine::new(EngineConfig::spawned())?;
let client = engine.client();
let handle = client.start(Request::get("https://example.com/").build()?, |result| {
    match result {
        Completion::Completed(response) => println!("HTTP {}", response.status()),
        Completion::Failed(error) => eprintln!("request failed: {error}"),
        Completion::Cancelled => eprintln!("request cancelled"),
        _ => {}
    }
})?;

// Idempotent if completion has already won.
handle.cancel()?;
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Use `Client::submit` when the calling code wants a unique direct waiter instead. `PendingRequest`
can be polled, waited with a caller-local timeout, or given to a manual Engine's `drive_until`.
A waiter timeout does not cancel its request.

## Cancellation

Each accepted request has a cloneable cancellation-only `RequestHandle`. `cancel` is idempotent
after a terminal result and never recalls bytes already accepted by the operating system or acted
on by a remote server. `CancelOnDrop` is useful when cancellation should follow a local scope.

`Engine::cancel_all` cancels the entire Engine domain. It is appropriate during service shutdown;
independent cancellation domains should use independent Engines.

## Streaming responses and uploads

Every `StreamRequest` has a streaming response and returns one unique `ResponseReader`. A buffered
body remains replayable; `body_stream` consumes one unique `UploadBody` and redirects are returned
unfollowed once a live upload is involved.

```rust,no_run
use nbreq::{Engine, EngineConfig, StreamRequest, UploadBody};

let engine = Engine::new(EngineConfig::spawned())?;
let client = engine.client();

let (body, mut sender) = UploadBody::chunked(256 * 1024)?;
let request = StreamRequest::post("https://example.com/upload")
    .header("Content-Type", "application/octet-stream")
    .body_stream(body)
    .build()?;
let reader = client.submit_stream(request)?;

sender.push(vec![0_u8; 64 * 1024])?;
sender.finish()?;
let response = reader.collect()?;

println!("HTTP {}", response.status());
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`UploadBody::fixed` generates `Content-Length` and requires exactly the declared byte count before
`finish`. `UploadBody::chunked` generates HTTP/1.1 chunk framing and permits an unknown total.
`try_push` never blocks and returns ownership of a refused chunk. Spawned-mode `push` admits large
chunks progressively and wakes on capacity, early response, cancellation, failure, or Engine stop.

`ResponseReader::wait_head`, `read`, and `collect` block only in spawned mode. Their `try_*`
counterparts are passive and are suitable for manual driving. `collect` is valid only before any
body byte has been consumed. Dropping a reader before known EOF requests cancellation; dropping it
after final EOF or a no-body response is harmless.

## Errors and TLS diagnosis

Use the structured fields on `Error` for decisions and treat `message()` as a payload-free human
diagnostic. In particular, `transport_stage()` identifies DNS, connect, TLS, send, receive, or HTTP
framing, while `tls_failure()` can distinguish safe categories such as hostname mismatch, unknown
issuer, expiry, peer alert, protocol failure, and local TLS I/O. Both enums are non-exhaustive, so
portable callers must retain a fallback arm:

```rust
use nbreq::{Error, TlsFailure};

fn tls_hint(error: &Error) -> &'static str {
    match error.tls_failure() {
        Some(TlsFailure::CertificateHostnameMismatch) => "check the requested hostname",
        Some(TlsFailure::CertificateUnknownIssuer) => "check the installed trust roots",
        Some(TlsFailure::CertificateExpired) => "renew the server certificate",
        Some(_) => "inspect the TLS category and deployment",
        None => "this was not a classified TLS failure",
    }
}
```

NBReq deliberately provides no raw-TLS-diagnostic switch: backend-native certificate errors can
contain requested or presented names. The structured category preserves operational usefulness
without making ordinary logging a data-disclosure path.

## Manual driving and GUI loops

Manual mode creates no reactor thread and dispatches callbacks inline only from explicit drive
calls. The unique Engine owner must call `drive`; Client methods and readers never drive it
implicitly:

```rust,no_run
use nbreq::{Completion, EngineBuilder, Request};

let mut engine = EngineBuilder::manual().build()?;
let client = engine.client();
let pending = client.submit(Request::get("https://example.com/").build()?)?;

match engine.drive_until(pending)? {
    Completion::Completed(response) => println!("HTTP {}", response.status()),
    Completion::Failed(error) => eprintln!("request failed: {error}"),
    Completion::Cancelled => eprintln!("request cancelled"),
    _ => {}
}
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

For a GUI with spawned networking, keep the Engine in an application service and have callbacks
send owned results through the GUI framework's own message/channel mechanism. Do not perform long
GUI work on NBReq's callback worker. Manual mode is useful only when the host can integrate regular
`drive` calls and accepts that delaying them delays all network progress.

## Shutdown, DLLs, and FFI ownership

`Engine::shutdown` consumes the unique owner, rejects new work, cancels accepted requests, stops and
joins network/resolver work, seals callback admission, and waits for callbacks. A callback that is
itself currently running can therefore delay ordinary shutdown.

`shutdown_for` can detach only the already-network-free callback domain and return
`ShutdownOutcome::CallbacksRemaining`. Keep the resulting `DetachedCallbacks` handle and wait for
it before unloading a module containing callback code. The handle owns no Engine, socket, resolver,
TLS, or backend state.

An FFI layer should expose opaque ownership handles: one unique Engine/service handle and separate
cloneable Client/request-control handles. Destroy consumer objects, stop the Engine, resolve any
detached callback handle, and only then permit library unload. Never initialize networking from a
Windows `DllMain` loader callback.

## Limits, pools, and metrics

Engine construction owns all resource ceilings: inflight requests, command and callback queues,
request/response bodies and headers, stream windows and aggregate queued bytes, active connections,
idle connections, and idle lifetime. Tighten them for the application; do not create an unbounded
Client-specific escape hatch.

`Engine::metrics` is a nonblocking, approximate, payload-free snapshot. Request and bounded-queue
metrics are portable. Check `connection_metrics_available` before interpreting physical
connection/pool counters; the native owner supplies them, while internal non-networking test
backends report honest unavailable zeroes.

## Backend and feature selection

- Default features: native backend, ordinary `Engine::new` and unqualified builder construction.
- `--no-default-features`: compiles the public types, but ordinary network construction returns
  `Unsupported`.
- `test-support`: deterministic downstream conformance controls, not required by consumers.

The pre-release curl Multi pilot is not part of the public crate feature matrix. It required a
locally patched binding and remains project-history/reference evidence rather than a supported
transport choice.

The first public package will document its exact platform matrix. The accepted baseline is Windows
10 or later and native Linux against an Ubuntu 20.04 ABI baseline; the Windows x86 GDS build also
passes its controlled compatibility workload under stock Wine 5.
