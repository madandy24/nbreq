# Upgrading to NBReq 0.2.1

Select `nbreq = "0.2.1"` for the APIs described here. Existing `"0.2"` requirements permit
0.2.1, but an application's lockfile controls when it updates; `"0.1"` stays on the 0.1 line.

## From 0.2.0 to 0.2.1

Existing HTTP, DNS and plain TCP APIs remain available. The additions are verified TLS 1.2/1.3
connections, consuming TCP-to-TLS upgrades, TLS waiters/callbacks, `Engine::run_mode()` and
explicit Engine trust selection. `PendingTlsConnect` works with the existing sealed
`WaiterTarget` and `drive_until`. `TlsFailure::Truncated` extends an already non-exhaustive enum;
retain wildcard match arms. Use a minimum of 0.2.1 when calling these new APIs.

Review these behavior changes when upgrading:

- Additional DER roots now pass WebPKI trust-anchor parsing even in platform mode. Certificates
  an OS previously accepted or ignored differently can fail Engine construction.
- Backends that cannot apply explicit trust settings return `Unsupported`. This includes public
  `test-support` held/HTTP-only constructors that previously ignored additional roots. Tests using
  production TLS settings may need an HTTPS fixture or a configuration without unused roots.
- Accepted plain TCP send buffers shed spare `Vec` capacity, which may allocate or copy. Refused
  buffers retain their original allocation and bytes; partial blocking sends still return only
  the unaccepted suffix.
- Peer FIN remains EOF when queue pressure changes, so read inactivity does not restart after
  EOF. Small native receive windows use bounded allocation. These fixes preserve byte-stream
  semantics while tightening resource behavior.

Platform trust remains the default, including when `bundled-roots` is enabled. Select
`TlsTrust::SuppliedRootsOnly` with at least one DER root, or enable `bundled-roots` and select
`TlsTrust::BundledMozilla`. Portable modes retain certificate and identity checks but do not
inherit OS enterprise roots, distrust rules or revocation retrieval. There is no automatic
fallback. Update the application lockfile, rebuild and redeploy to refresh bundled roots.
See [trust configuration and TLS lifecycle](https://github.com/madandy24/nbreq/blob/v0.2.1/docs/tcp-tls.md)
and [runnable TLS examples](https://github.com/madandy24/nbreq/blob/v0.2.1/examples/README.md#c--tcp).

Rust 1.85 remains supported with compatible dependency selection. Fresh graphs
can select upstream `yoke-derive 0.8.3`, which needs a Rust 1.87 API; older-compiler
applications can select 0.8.2 in their own lockfile without restricting all NBReq
consumers. See the [commands and dependency policy](https://github.com/madandy24/nbreq/blob/v0.2.1/docs/getting-started.md#rust-version-and-dependency-selection).

The following sections describe the earlier changes from 0.1.1 to the 0.2 line.

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
  the native reactor and does not expose raw sockets. Version 0.2.1 adds separate verified TLS
  connection and upgrade calls without changing the ordinary cleartext calls.
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
