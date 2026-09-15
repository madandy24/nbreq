# NBReq

HTTP, DNS and TCP for Rust applications that need concurrent networking, prompt cancellation and
control over resource use. Start with a blocking request, submit work to a background engine, or
drive networking from your own event loop. No async runtime required.

## Highlights

- Simple blocking HTTP requests for ordinary use, with callbacks, direct waiters, streaming
  uploads, and streaming responses for advanced scenarios.
- Prompt cancellation across DNS, connection, TLS, upload, and download work. Shutdown closes
  sockets before joining owned workers; an executing platform certificate check may delay completion.
- Run networking on an owned background thread, or drive it manually from a single thread.
- No Tokio or other async runtime required.
- Bounded queues, resource limits, connection pooling, structured errors, and deterministic joined
  shutdown.
- Rust-native HTTP/1.1 and TLS on Windows, Linux, and Intel/Apple Silicon macOS.
- Public DNS resolution and cleartext TCP connections using the same Engine ownership and shutdown.
- Shared buffered responses, optional consuming buffer transfer, and opt-in retained-body limits.

## Start with a GET

Requires Rust 1.85 or newer. This guide covers NBReq 0.2.0.

```toml
[dependencies]
nbreq = "0.2"
```

```rust
use std::time::Duration;
use nbreq::Engine;

let engine = Engine::builder().build()?;
let response = engine
    .get("https://httpbin.org/get")
    .total_timeout(Duration::from_secs(15))
    .call()?;

println!("HTTP {}", response.status());
println!("{}", String::from_utf8_lossy(response.body()));
engine.shutdown()?;
```

The snippets use `?` inside a function returning `Result`. For a complete program, start with
[A01: blocking GET](examples/A01-http-blocking-get.rs), then try
[A02: blocking POST](examples/A02-http-blocking-post.rs).

## More control when you need it

- **Choose how to run.** Blocking calls, nonblocking submission and callbacks share the same
  request path. Manual driving lets an existing application loop own network progress.
- **Own the lifecycle.** One `Engine` owns network work, connection pools and workers. Cheap
  `Client` handles let other parts of your application submit requests. Explicit shutdown cancels
  outstanding work and joins the owned workers.
- **Bound your workload.** Configure connection and request limits, body budgets and queue sizes.
  Stream larger bodies incrementally; share buffered responses or take their allocation without
  copying when uniquely owned. [Memory controls](docs/getting-started.md#buffered-http-memory-controls)
  and [A10: memory limits](examples/A10-http-memory-limits.rs) explain the choices.
- **Use verified HTTPS.** TLS certificate and hostname verification is enabled by default, using
  platform trust. Applications can also supply private CA roots.

Keep an Engine for the lifetime of a service so requests can reuse its connections and DNS cache.
The following snippets each assume a running, spawned `engine` and `Duration` as above.

### Cancel outstanding work

```rust
use nbreq::{Completion, Request};

let pending = engine.client().submit(
    Request::get("https://httpbin.org/get")
        .total_timeout(Duration::from_secs(15))
        .build()?,
)?;

// The application can do other work, then decide it no longer needs the request.
pending.handle().cancel()?;
match pending.wait() {
    Completion::Cancelled => println!("Request cancelled"),
    other => println!("Request finished before cancellation: {other:?}"),
}
```

Check the terminal result: completion can win a race with cancellation. Use `engine.cancel_all()`
to cancel all outstanding work while keeping the Engine available for new requests.
[A07: cancellation](examples/A07-http-cancel.rs) uses a local server to demonstrate and verify
`Cancelled` after the server has received the request.

### Resolve a hostname

```rust
use nbreq::ResolveRequest;

let answer = engine.resolver().execute(
    ResolveRequest::hostname("example.com")
        .total_timeout(Duration::from_secs(10))
        .build()?,
)?;

println!("DNS {:?}", answer.status());
for address in answer.addresses() {
    println!("{}", address.address());
}
```

Choose address families, ordering, caching and search-suffix behavior when needed. DNS also
supports nonblocking submission, callbacks, cancellation and manual driving.
See the [DNS examples](examples/README.md#b--dns).

### Exchange bytes over TCP

With an echo server listening on `127.0.0.1:9000`:

```rust
use nbreq::TcpConnectRequest;

let request = TcpConnectRequest::literal("127.0.0.1:9000".parse()?)
    .connect_timeout(Duration::from_secs(5))
    .read_inactivity_timeout(Duration::from_secs(10))
    .write_inactivity_timeout(Duration::from_secs(10))
    .build()?;
let mut connection = engine.tcp_connector().execute(request)?;

connection.send(b"hello\n".to_vec())?;
connection.finish()?; // Half-close our output; keep reading the reply.
let mut buffer = [0_u8; 256];
while let Some(count) = connection.read(&mut buffer)? {
    println!("Received {count} bytes");
}
```

TCP supports hostname connections, separate reader/writer halves, cancellation, bounded queues
and nonblocking I/O. The [TCP examples](examples/README.md#c--tcp) start their own local echo server,
so you can run them without setting one up.

## Learn more

- [Getting started](docs/getting-started.md): request options, streaming, manual driving,
  memory controls, error handling and embedding in GUI/FFI applications.
- [17 runnable examples](examples/README.md): **A** HTTP, **B** DNS, **C** TCP; simplest first.
- [Migrating from 0.1.1](docs/migrating-to-0.2.md): changes to response ownership and resource limits.

## Scope and configuration

NBReq provides native HTTP/1.1 and HTTPS, DNS resolution and cleartext TCP on Windows, Linux,
and Intel/Apple Silicon macOS. TCP does not add TLS or message framing. macOS currently supports
the ordinary default DNS configuration; richer split/scoped configurations are rejected explicitly.
See the [platform scope](docs/getting-started.md#platform-scope) for tested versions and Wine coverage.

HTTP 4xx/5xx statuses are responses; transport, timeout, limit and cancellation failures are
separate outcomes. Resource budgets cover NBReq's specified resources, not total process RAM.
Shutdown joins owned work; executing callbacks or platform certificate checks can delay it.
The [guide](docs/getting-started.md) covers these contracts in detail.

Default features include HTTP, HTTPS, TCP and the public DNS resolver. HTTP/TCP consumers can
use `default-features = false, features = ["native"]` to omit the public Resolver while retaining
internal DNS. See [feature selection](docs/getting-started.md#backend-and-feature-selection).

Runtime dependencies use Cargo-compatible version ranges. Your application's lockfile controls
updates; release checks cover locked and fresh consumer graphs on stable Rust and the minimum
supported version. See [dependency policy and security reporting](SECURITY.md).

## Project and license

NBReq is developed by [Cave Rock Software Limited](https://www.caverock.com/), and is licensed under
either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option. The generated
[component and dependency license report](THIRD_PARTY_LICENSES.html) records the locked release graph.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.

Copyright (c) 2026 Cave Rock Software Limited.

## History

An early prototype used curl as a comparison backend. The supported crate uses NBReq's native
implementation; the prototype remains available in Git history.
