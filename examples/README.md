# Runnable examples

Each numbered source file is a complete program. Read each group from the top: A is HTTP,
B is DNS, and C is cleartext TCP. The default Cargo features enable all three groups.
Run these commands from the NBReq repository with Rust 1.85 or newer:

```sh
cargo run --example A01-http-blocking-get
cargo run --example A02-http-blocking-post
cargo run --example A07-http-cancel
cargo run --example B01-dns-blocking
cargo run --example C01-tcp-blocking
```

## A — HTTP

| Program | Main idea | Default destination / expected result |
| --- | --- | --- |
| [A01-http-blocking-get](A01-http-blocking-get.rs) | Convenience GET | httpbin `/get`; status and response body |
| [A02-http-blocking-post](A02-http-blocking-post.rs) | Convenience POST | httpbin `/post`; echoed text body |
| [A03-http-full-get](A03-http-full-get.rs) | Explicit Request, timeouts, headers, Engine reuse | Two GETs, statuses and headers |
| [A04-http-full-post](A04-http-full-post.rs) | Explicit JSON POST and request/response limits | Echoed JSON body |
| [A05-http-nonblocking](A05-http-nonblocking.rs) | Submit two requests before collecting results | Two terminal responses |
| [A06-http-callbacks](A06-http-callbacks.rs) | Hand completion from a callback to the application | Status delivered through a channel |
| [A07-http-cancel](A07-http-cancel.rs) | Cancel a request already received by a server | Local server; verifies `Completion::Cancelled` |
| [A08-http-manual](A08-http-manual.rs) | Interleave host work with `Engine::drive` | GET completed by the application's loop |
| [A09-http-streaming](A09-http-streaming.rs) | Chunked upload and incremental response reading | httpbin `/post`; streamed response bytes |
| [A10-http-memory-limits](A10-http-memory-limits.rs) | Budgets, limit errors and taking body ownership | GET plus retained-capacity observation |
| [A11-http-owner-lifecycle](A11-http-owner-lifecycle.rs) | Service ownership and explicit shutdown | GET; detached Client rejects new work |

These are runtime-independent APIs: nonblocking submission, waiters, callbacks and manual driving,
not Rust `async`/`.await` examples. Blocking terminals belong to spawned Engines. A Client is a cheap
command handle; it does not keep the owning Engine alive. Reuse the Engine across requests.

HTTP examples use verified HTTPS at [httpbin](https://httpbin.org/), a request/response demonstration
service. They send only small illustrative payloads. Public-service availability is outside NBReq's
control. Every HTTP example except the deterministic local cancellation example accepts a URL:

```sh
cargo run --example A01-http-blocking-get -- https://example.com/
cargo run --example A02-http-blocking-post -- http://127.0.0.1:8080/post
```

Use a POST-capable endpoint for A02/A04/A09. Example.com is an IANA documentation domain, not a POST
echo service. HTTP 4xx/5xx still produce a Response; inspect the status separately from transport
errors. Keep TLS verification enabled. The cancellation example waits for local receipt, requests
cancellation, and requires the terminal `Cancelled` result; a successful `cancel()` alone does not
prove cancellation won a race with completion. It accepts no destination override.

A09 finishes a small upload before reading the response. For a large bidirectional exchange,
interleave upload and download (or use separate producer/consumer threads) so both bounded queues
can make progress. A10's illustrative limits cover NBReq resources, not total application RAM.

## B — DNS

| Program | Main idea |
| --- | --- |
| [B01-dns-blocking](B01-dns-blocking.rs) | Resolve a name and distinguish answers, NXDOMAIN and NoData |
| [B02-dns-nonblocking](B02-dns-nonblocking.rs) | Configure families/order/cache, submit, handle a local wait timeout |
| [B03-dns-manual](B03-dns-manual.rs) | Drive a resolver waiter with a manual Engine |

All three default to `example.com`, accept an optional hostname, and print the DNS status and any
addresses. Results use the machine's configured DNS servers; address values and ordering can vary.
Search suffixes are opt-in. A valid negative answer is distinct from a transport failure.

```sh
cargo run --example B02-dns-nonblocking -- example.org
```

## C — TCP

| Program | Main idea |
| --- | --- |
| [C01-tcp-blocking](C01-tcp-blocking.rs) | Connect, send, half-close, read through EOF |
| [C02-tcp-nonblocking](C02-tcp-nonblocking.rs) | Submit connect, split the connection, retry refused bytes and drain both directions |
| [C03-tcp-manual](C03-tcp-manual.rs) | Drive connect and connected I/O with `try_*` calls |

With no arguments, each starts a one-client echo server on an ephemeral loopback port. Successful
runs print `echoed 17 bytes and received EOF`. The server helpers in [support](support/echo.rs) contain
only fixture plumbing; the client operations are visible in each example. TCP is cleartext and does
not expose a TLS mode. The nonblocking example uses small polling intervals, not a busy loop.

To use your own echo server, C01/C03 accept `IP:PORT`; C02 accepts `HOSTNAME PORT` and resolves that
name through NBReq. Its default loopback address skips DNS.

```sh
cargo run --example C01-tcp-blocking -- 127.0.0.1:9000
cargo run --example C02-tcp-nonblocking -- echo.example.org 9000
```

The hostname above is a placeholder for your own server. Sending FIN closes only our write side;
we keep reading until the peer sends EOF. Dropping an unfinished connection aborts it.

## Building and checking

```sh
cargo build --examples
cargo build --examples --no-default-features --features native
```

The second command builds HTTP/TCP and skips the DNS examples, which require `resolver`. The source
files, this index and local server helpers are included in the crate package. Repository CI also
executes HTTP/TCP against local fixtures; optional live HTTPS/DNS smoke checks are recorded separately.
