# Using NBReq

For complete programs in learning order, see the [HTTP, DNS and TCP examples](../examples/README.md).

NBReq is built around one explicit owner. An `Engine` owns network state, pools, DNS work, callback
workers, limits, and shutdown. It issues cheap cloneable `Client` command handles, but a Client
neither owns nor extends the Engine's lifetime. Keep the Engine in the service or module that is
responsible for stopping HTTP.

The default Cargo features are `native` and `resolver`. They provide NBReq's Rust-native HTTP/1.1,
DNS, TCP, public Resolver, and rustls implementation without Tokio or another async runtime.

This guide targets NBReq 0.2. Add the dependency below to use the default native backend:

```toml
[dependencies]
nbreq = "0.2"
```

An HTTP-only consumer can omit the public Resolver API and its Windows search-suffix registry
reader while retaining native HTTP and exact-name DNS for HTTP and hostname `TcpConnector`:

```toml
[dependencies]
nbreq = { version = "0.2", default-features = false, features = ["native"] }
```

The `resolver` feature implies `native`. Turning it off does not use a blocking OS resolver and does
not create a different DNS owner; it only removes the public Resolver surface and search expansion.

## Basic buffered requests

Spawned mode owns its reactor thread and supports simple Engine-bound blocking GET and POST calls.
The convenience builder delegates to the ordinary Request and Client path, so it uses the same
backend, pool, limits, timeouts, redirects, TLS policy, errors, and shutdown owner:

```rust,no_run
use std::time::Duration;

use nbreq::{Engine, EngineConfig};

let engine = Engine::new(EngineConfig::spawned())?;

let response = engine
    .get("https://example.com/")
    .header("Accept", "text/plain")
    .total_timeout(Duration::from_secs(30))
    .call()?;

let created = engine
    .post("https://httpbin.org/post")
    .header("Content-Type", "application/json")
    .total_timeout(Duration::from_secs(30))
    .send(br#"{"name":"thing"}"#)?;

println!("GET {}, POST {}", response.status(), created.status());
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`call()` sends the body already configured on the builder and is valid for an empty POST.
`send(body)` replaces that buffered body; `send_empty()` explicitly replaces it with an empty body.
HTTP 4xx and 5xx statuses remain ordinary `Response` values. These blocking terminals reject a
manually driven Engine with `WrongMode` rather than driving it implicitly.

## Buffered response ownership

`response.body()` still borrows a byte slice, and `response.body().to_vec()` explicitly copies it
into independent application storage. Cloning a `Response` now shares its immutable body bytes;
headers are still cloned. Completion delivery moves the response to its waiter or callback.

Use consuming access to take the original buffer when it has no other owners:

```rust
use nbreq::Response;

let response = Response::new(200, Vec::new(), b"hello".to_vec());
let body = response.into_body();
let bytes = match body.try_into_vec() {
    Ok(bytes) => bytes, // Original allocation, including spare capacity; no byte copy.
    Err(shared) => shared.as_bytes().to_vec(), // An explicit application choice to copy.
};
let text = String::from_utf8(bytes)?; // Reuses the Vec allocation for valid UTF-8.
assert_eq!(text, "hello");
# Ok::<(), std::string::FromUtf8Error>(())
```

`ResponseBody` is cloneable, immutable and shareable between threads. Its `as_bytes()` and
`AsRef<[u8]>` accessors borrow bytes. `try_into_vec()` returns the original body owner in `Err`
while any other response/body shares that allocation; it never silently copies. Instead of
copying, callers can drop their other owners and retry. Taking a Vec transfers responsibility
for its memory to the application. It does not free RAM. A retained body can outlive Engine
shutdown without keeping sockets or workers alive.

These APIs also define how the optional aggregate buffered-body budget retains and releases
charges. See the memory controls below. The API does not promise zero-copy network or TLS I/O.

## Spawned mode and explicit blocking requests

Issue a cheap cloneable Client when code needs the explicit Request surface or will later use
callbacks, direct waiters, cancellation handles, or streaming. The Client does not own or extend
the Engine lifetime:

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

## Private certificate authorities

Add DER-encoded root certificates to an Engine when an application uses a private CA. The roots
supplement platform trust, without modifying the operating-system trust store. Hostname, validity
and signature verification stay enabled:

```rust,no_run
use nbreq::{Engine, EngineConfig};

let root_der = std::fs::read("company-root.der")?;
let config = EngineConfig::spawned().with_additional_tls_root_certificate(root_der);
let engine = Engine::new(config)?; // Rejects malformed certificates during construction.
let response = engine.get("https://internal.example/").call()?;
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Call the configuration method once per root. An Engine's trust policy is fixed at construction and
applies to all its HTTPS requests and redirects; use separate Engines for separate trust domains.
Windows, Linux and macOS use the existing platform verifier with extra roots. The pinned Android
verifier does not support this option and rejects a nonempty extra-root configuration explicitly.

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
let request = StreamRequest::post("https://httpbin.org/post")
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

### Ambiguous HTTP response framing

The native HTTP/1.1 backend rejects a response containing both `Transfer-Encoding` and
`Content-Length`, even when a legacy server intended a valid chunked body. The result is a
transport error at the HTTP stage. This avoids accepting ambiguous framing that can cause
response splitting or disagreement between intermediaries; no permissive compatibility switch
is provided. See [RFC 9112 section 6.3](https://httpwg.org/specs/rfc9112.html#message.body.length).

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

Native TLS handshake processing uses an Engine-owned service, started lazily, with at most two
workers and four queued steps. These workers process supplied TLS bytes; sockets and application
callbacks stay with their existing owners. Established TLS traffic does not use the worker queue.
In manual mode, worker completion becomes network progress only on a subsequent `drive` call.
Saturated workers delay new handshakes within the existing connection limits and request deadlines.

## Shutdown, DLLs, and FFI ownership

`Engine::shutdown` consumes the unique owner, rejects new work, cancels accepted requests, stops and
joins network/resolver/TLS-worker work, seals callback admission, and waits for callbacks. A
callback that is itself currently running can therefore delay ordinary shutdown.

Cancellation and request deadlines close the request's NBReq socket without waiting for an
executing platform certificate check. That check may not be interruptible: its worker and TLS
state remain owned and bounded until it returns. Shutdown discards queued handshake work and
closes sockets before joining executing checks, so a slow platform check can delay shutdown.

`shutdown_for` can detach only the already-network-free callback domain and return
`ShutdownOutcome::CallbacksRemaining`. Keep the resulting `DetachedCallbacks` handle and wait for
it before unloading a module containing callback code. The handle owns no Engine, socket, resolver,
TLS, or backend state.

The `shutdown_for` duration bounds callback draining after network shutdown; it does not set a
deadline for joining executing platform certificate checks.

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

## DNS resolution

With the default-on `resolver` feature, `Engine::resolver()` issues a cheap cloneable ticket
into the Engine's existing DNS service. Use `execute` in spawned mode, `submit` for a direct
waiter, or `start` for a callback. A valid negative DNS answer is a `ResolveResponse`, not a
transport error:

```rust,no_run
# #[cfg(feature = "resolver")]
# {
use std::time::Duration;
use nbreq::{Engine, ResolveRequest, ResolveStatus};

let engine = Engine::builder().build()?;
let answer = engine.resolver().execute(
    ResolveRequest::hostname("example.com")
        .total_timeout(Duration::from_secs(10))
        .build()?,
)?;
match answer.status() {
    ResolveStatus::Answer => {
        for address in answer.addresses() {
            println!("{}", address.address());
        }
    }
    ResolveStatus::NameNotFound => println!("name does not exist"),
    ResolveStatus::NoData => println!("no addresses in the requested families"),
    _ => {}
}
engine.shutdown()?;
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

Names must be ASCII or already punycode-encoded. Lookups are exact by default; search-suffix
expansion is explicit through `use_search_suffixes(true)`, and a trailing dot keeps the lookup
absolute. This is a DNS API, not a replacement for every OS name service such as mDNS or hosts-file
lookup. `AddressFamily::Both` collects A and AAAA; failure of either required family fails the
operation instead of presenting a partial answer. It does not promise Happy Eyeballs.

`CacheMode::Use` reads/populates the shared Engine cache; `Refresh` skips the read and replaces
the network result, and `Bypass` neither reads nor populates it. Public refreshes can affect later
HTTP lookups. HTTP and hostname TCP remain exact-name even when public search expansion is enabled.
In manual mode, pass a submitted `PendingResolve` to `engine.drive_until(pending)` or drive and
poll it explicitly. Blocking `execute` rejects manual mode. Direct waiters never drive the
Engine: use `wait_for(Duration::ZERO)` to poll and recover a pending waiter from `TimedOut`;
calling `wait()` on its undriven owner thread cannot make network progress.

## Cleartext TCP connections

`Engine::tcp_connector()` provides literal-address and exact-hostname connects on the same
reactor. It is available with `native`, even when the public Resolver feature is disabled.
It provides a byte stream, with no TLS wrapping or message framing:

```rust,no_run
use std::time::Duration;
use nbreq::{Engine, TcpConnectRequest};

let engine = Engine::builder().build()?;
let request = TcpConnectRequest::literal("127.0.0.1:9000".parse()?)
    .connect_timeout(Duration::from_secs(5))
    .read_inactivity_timeout(Duration::from_secs(10))
    .write_inactivity_timeout(Duration::from_secs(10))
    .send_queue_bytes(16 * 1024)
    .receive_queue_bytes(16 * 1024)
    .build()?;
let mut connection = engine.tcp_connector().execute(request)?;
connection.send(b"hello\n".to_vec())?;
connection.finish()?; // Drain output and half-close; keep reading the reply.
let mut buffer = [0_u8; 1024];
while let Some(count) = connection.read(&mut buffer)? {
    println!("received {count} bytes");
}
drop(connection);
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

This example expects a local server that replies and closes after client EOF. For a hostname,
use `TcpConnectRequest::hostname("server.example", port)`; address attempts are serial, not
parallel Happy Eyeballs. Connect timeout includes queueing and DNS. Read/write inactivity timers
apply after connection; consumer backpressure pauses read inactivity, and write inactivity runs
only while accepted output waits for socket progress.

`try_send` returns refused input unchanged; blocking `send` can accept a prefix before failing,
so `TcpSendError::into_remaining()` returns only the unaccepted suffix. Retrying the original whole
message can duplicate its accepted prefix. A successful send means bytes were queued, not that
the peer processed them. Bound each chunk to the configured send window for passive `try_send`.

For manual driving use `submit`/`drive_until` to connect and passive `try_read`, `try_send` and
`try_finish` between drive calls. Blocking connected methods reject manual mode. `split` moves
one connection into unique reader/writer halves; each is Send but not Sync. Dropping a reader
before EOF or a writer before requesting finish aborts the connection. A successful `try_finish`
or `finish_with` requests drain-then-half-close; the split writer may then be dropped, but the
reader still needs to drain to EOF. Dropping an unsplit connection before EOF also aborts it.
Use a cloned connection handle to cancel from another thread.

## Backend and feature selection

| Cargo selection | Available behavior |
| --- | --- |
| Default | Native HTTP/1.1, TLS, internal DNS, standalone TCP and public Resolver |
| `default-features = false, features = ["native"]` | Native HTTP/TLS/TCP and internal exact-name DNS; no public Resolver or Windows search-suffix registry reader |
| No features | Portable configuration/HTTP/TCP types compile; public Resolver is absent and Engine construction returns `Unsupported` |
| `test-support` | Additional deterministic test controls; does not select a network backend or add production capabilities |

The historical curl Multi pilot is not part of the public crate feature matrix. It required a
locally patched binding and remains project-history/reference evidence rather than a supported
transport choice.

## Platform scope

NBReq 0.2 targets Rust 1.85 or later with Rust 2024 edition. The verified target set is:

| Target | Tested scope |
| --- | --- |
| Windows x64 MSVC | Windows 10 or later; native HTTP/TLS, DNS, TCP and lifecycle gates |
| Linux x64 GNU | Ubuntu 20.04 ABI baseline; native HTTP/TLS, DNS, TCP and lifecycle gates |
| macOS Intel and Apple Silicon | macOS 15 CI on both architectures, physical Intel macOS 15 and Apple Silicon macOS 26; ordinary default DNS topology, Keychain trust and lifecycle gates |
| Windows x86 MSVC | Additional focused memory/lifecycle and consumer-integration evidence; not a claim of every x64 test on x86 |

Other operating systems, architectures and older macOS versions are not covered by this release's
support claim. Windows x86 also has focused compatibility evidence under Ubuntu 20.04's stock
Wine 5. This does not establish support for every Wine/host combination.

macOS discovery accepts a bounded ordinary default System Configuration view. Supplemental
`/etc/resolver` entries, split/scoped routing, conflicting primary services and other unrepresented
topologies return `Unsupported` instead of sending DNS queries to a guessed server. This can reject
Engine construction, including a native-only HTTP consumer; disabling the public Resolver does
not bypass internal DNS discovery. Accepted system configuration changes are rediscovered by the
existing DNS owner. Windows/Linux discovery likewise uses the Engine's bounded configuration
model; NBReq does not promise every OS resolver extension.

## Buffered HTTP memory controls

Engine body ceilings remain 16 MiB by default. A request can further restrict either ceiling
using `max_request_body_bytes` and `max_response_body_bytes`, or the corresponding optional
fields in `RequestOptions`. `None` inherits the Engine ceiling; zero allows only an empty body.
These limits follow redirects and apply to total streaming bodies as well as buffered bodies.
They are separate from streaming queue windows. An upload is checked before admission; a reply
is checked at validated framing and during receipt, before buffering an oversized body.

An optional `EngineBuilder::max_buffered_body_bytes` / `EngineConfig::with_max_buffered_body_bytes`
caps aggregate retained buffered payload capacity. No aggregate cap is enabled by default.
For example (values are illustrative, not a recommended device profile):

```no_run
use nbreq::Engine;

let engine = Engine::builder()
    .max_buffered_body_bytes(8 * 1024 * 1024)
    .build()?;
let response = engine.get("https://example.com/status")
    .max_response_body_bytes(1024 * 1024)
    .call()?;
// Borrow or explicitly copy response.body(), as before. Response clones share one charge.
let body = response.into_body();
let application_bytes = body.try_into_vec().expect("no other body owner");
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The aggregate scope includes admitted request Vec capacity (also buffered uploads with streamed
responses), returned/unread/queued response bodies, spare capacity, transient old and new body
allocations during growth, receive-event storage and extracted TLS plaintext. Cleartext buffered
uploads queue shared slices of the original allocation, so they need no copied body send buffer.
Receive windows are conservatively charged in full even when they also contain HTTP framing.
Separately parsed headers and other metadata remain governed by their existing limits. TLS
session/record output buffers, socket buffers, stacks, allocator bookkeeping/rounding and
application-owned allocations need additional headroom. This is not a process RAM cap.

Known response lengths reserve capacity at the head without allocating it until payload arrives.
HEAD/no-body responses do not reserve the advertised representation size. Unknown-length bodies
grow in bounded steps, acquiring the replacement's capacity while the old allocation is still
charged. The budget must therefore leave room for staging and growth, not just final body length.
There is no full maximum-response reservation for each queued request or quiet long poll.

Insufficient capacity produces `ErrorKind::Limit` with `LimitKind::BufferedBodyBytes`, distinct
from `RequestBodyBytes` / `ResponseBodyBytes`. NBReq fails the exchange rather than waiting for
other partial replies, and never automatically replays it. A response-side failure may occur
after a POST has taken effect; the error does not imply that retrying is safe.

Completed responses remain charged until the last body owner drops, including after Engine
shutdown. Successful unique `try_into_vec` transfers the existing allocation into application
ownership and ends its NBReq charge; the memory still exists and must be budgeted by the caller.
Public `Response::new` and explicit streaming collection create application-owned bodies.
The ledger retains no sockets or workers. `engine.metrics().current().reserved_buffered_body_bytes()`
reports retained/reserved capacity, and `high_water()` exposes its observed peak.

Configure concurrent-work admission as well as byte limits. The native defaults are 32 HTTP
connection slots, eight per origin and 1,024 accepted HTTP requests. These are ceilings, not
allocations made for every slot. Reduce accepted work for an application that can queue it more
cheaply elsewhere. Low per-origin connection counts can delay short calls behind long polls;
application scheduling should leave room for time-sensitive traffic.

There are three distinct byte controls:

| Control | Charged work |
| --- | --- |
| `max_buffered_body_bytes` (optional) | Retained/reserved buffered HTTP payload capacity and its charged receive staging |
| `max_stream_queued_bytes` | HTTP streaming response windows |
| `max_queued_bytes` | Shared parent for HTTP streaming response windows and reserved standalone TCP send/receive windows |

Per-stream and per-TCP-connection windows also apply. A buffered upload with a streamed response
participates in the buffered-body cap as well as its response's streaming window. These limits
do not include all application, TLS, kernel, thread-stack or allocator memory.
