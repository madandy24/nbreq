# Upgrading from NBReq 0.1.1 to 0.2

This document describes the public changes from 0.1.1 to 0.2. Select `nbreq = "0.2"` to upgrade;
a dependency on `"0.1"` intentionally stays on the 0.1 line.

## Existing HTTP consumers

The explicit `Engine` / `Client` / `Request` APIs, borrowed `Response::body()` slice, callbacks,
direct waiters, manual drive, streaming, cancellation and consuming shutdown remain available.
HTTP 4xx/5xx remain responses; cancellation, timeout, transport and limit errors remain distinct.
Neither Client handles nor retained response bodies extend the Engine's network lifetime.

`Response::clone()` now shares immutable body storage instead of copying its bytes; headers still
clone. Code reading `body()` works as before. Code requiring independent application storage can
continue to call `response.body().to_vec()`. Code relying on a clone allocating a fresh body must
make that copy explicit. Consuming access can transfer the original allocation:

```rust
use nbreq::Response;

let original = Response::new(200, Vec::new(), b"payload".to_vec());
let retained = original.clone();
let body = original.into_body();
let shared = body.try_into_vec().expect_err("retained still owns the body");
drop(retained);
let bytes = shared.try_into_vec().expect("now uniquely owned");
assert_eq!(bytes, b"payload");
```

An enabled aggregate buffered-body cap remains charged while any response/body owner retains the
allocation, including after Engine shutdown. Unique Vec transfer ends the nbreq charge and moves
memory responsibility to the application; it does not free that allocation.

## New capabilities

- `Engine::get` / `post` provide blocking buffered convenience on a spawned Engine. They share
  the ordinary pool, limits and lifecycle; no global runtime is created.
- `Engine::resolver` provides public DNS through the default-on `resolver` feature. Exact lookup
  is the default, search expansion is opt-in, and NXDOMAIN/NoData are completed negative answers.
- `Engine::tcp_connector` provides cancellable cleartext TCP and bounded duplex queues. It shares
  the native reactor; it is not a TLS stream or a `std::net::TcpStream` replacement with raw sockets.
- `Engine::drive_until` accepts HTTP, DNS and TCP direct waiters through the sealed `WaiterTarget`
  trait. Ordinary HTTP calls retain their return type; consumers storing the method as a function
  item may need to specify the waiter type explicitly.
- Per-request body ceilings can tighten Engine limits; an opt-in aggregate buffered-body cap
  accounts for retained capacity and staging. Both controls report distinct `LimitKind` values.
- Additional DER CA certificates supplement platform trust for an Engine. Hostname/validity checks
  remain enabled, and trust changes apply to that Engine's redirects as well.
- Intel and Apple Silicon macOS join the tested platform set for bounded ordinary-default DNS
  topology. Richer split/scoped configurations are rejected explicitly.

## Configuration and behavior to review

The default features are now `native` and `resolver`. Native-only consumers can disable the
public Resolver while retaining HTTP, TLS, standalone TCP and internal exact-name DNS. A build
with no features compiles portable types but cannot construct a network Engine. `test-support`
is opt-in test machinery, not a backend. The historical curl pilot remains absent from the crate.

Body defaults remain 16 MiB per request/response, and the aggregate buffered-body cap is disabled
unless configured. Individual limits also apply to streamed totals; stream queue windows govern
buffered chunks separately. Shared `max_queued_bytes` bounds reserved HTTP-streaming and TCP
windows, while `max_buffered_body_bytes` covers buffered HTTP storage. Neither is a process-RAM
limit. Application-specific adapters may choose different ceilings and workload-admission policies.

New `RequestOptions` fields default to inheritance. Construct it with `Default` and mutate fields,
or use request builders; it remains non-exhaustive. Continue using wildcard arms for public
non-exhaustive errors, completions, modes and policy enums.

Failure to reserve a response body can occur after the server has acted on a POST. Do not treat
`LimitKind::BufferedBodyBytes` as permission to replay it. Release retained data or reduce newly
admitted work, and use the application's own idempotency/recovery policy.

TLS handshake workers are lazy and bounded. Request cancellation closes its socket promptly,
but an executing platform certificate check can delay joined shutdown. `shutdown_for` bounds
callback draining after network shutdown; it is not a deadline for platform verification work.

The bounded internal DNS codec replaces the former general DNS dependency. Private DNS parsing,
address fallback, fragmented response handling, streaming shutdown and package-fixture fixes
are included without adding a second resolver/reactor owner. Existing 0.1.1 adapter-recovery
behavior is retained. macOS's `nbreq-darwin` helper is an implementation dependency, like
`nbreq-winpoll`; consumers should depend on nbreq itself.

## Upgrade check

Compile the application with its intended feature selection and Rust version. Exercise its
shutdown order, long polls, callback dispatch, retained responses and any configured limits.
For memory-constrained applications, size concurrent work and application-owned allocations
alongside nbreq's caps. The release's component measurements do not establish a universal
128/256 MB device profile or an end-to-end performance guarantee.
