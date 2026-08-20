# WP8 native DNS and TLS evidence

Status: **in progress — ownership seam frozen on 2026-08-20.** WP7 supplies the accepted socket,
deadline, cancellation, and HTTP/1.1 owner. This document records WP8 proof as it lands; it is not
yet a production-native or GDS-selection claim.

## Frozen ownership seam

- The `Engine` owns every resolver object, resolver socket, TLS configuration, TLS connection, DNS
  cache entry, and worker thread. No process-global runtime, detached lookup, or callback owns
  network work.
- DNS does not call blocking `getaddrinfo` on the reactor or hide it on an uninterruptible worker.
  A small NBReq-owned resolver service uses nonblocking sockets and an explicit waker. Cancellation
  removes a request's query immediately; shutdown wakes and joins the service before network-side
  teardown can report complete.
- The resolver service communicates only owned commands and results. It never receives a `Client`,
  callback, request waiter, HTTP parser, TLS connection, or reactor socket.
- The native backend remains the sole owner of request stage transitions. A request moves through
  resolve, connect, TLS handshake, request send, and response receive without changing its public
  `RequestId` or terminal arbitration.
- `rustls::ClientConnection` is owned and driven on the native backend owner. Encrypted bytes cross
  the existing reactor boundary; plaintext crosses only the private TLS/HTTP boundary. No async
  runtime or TLS worker owns a socket.
- Resolver and TLS proof constructors remain private test support. `Engine::new` does not select the
  native backend until WP8/WP9 parity and release-platform gates pass.

## Timeout and cancellation policy

- Total time starts at request acceptance and includes queueing, DNS, TCP connect, TLS, send, and
  receive.
- The portable connect timeout covers DNS, TCP connect, and TLS establishment. It stops only when
  the TLS handshake has completed for HTTPS, or TCP has connected for cleartext HTTP.
- Inactivity starts at acceptance. A valid DNS reply, successful TCP connect, encrypted socket
  progress, TLS plaintext progress, HTTP upload progress, and response progress refresh it. Merely
  retrying a timer or waking a thread does not.
- DNS failures map to `TransportStage::Dns`, TCP establishment to `Connect`, TLS configuration or
  handshake to `Tls`, and later encrypted transfer failures retain Send/Receive/Http meaning where
  the stage is known.
- Individual cancel, cancel-all, and shutdown abandon resolver results and close any connecting or
  TLS socket through the same exactly-once terminal path. Resolver shutdown is not callback
  detachment: all resolver threads must have exited before network shutdown completes.

## Dependency decision

- Pin `rustls` 0.23.42 with the Ring provider, standard library support, and TLS 1.2. It exposes the
  nonblocking TLS operations needed by the existing owner.
- Pin `rustls-platform-verifier` 0.7.0 for the eventual verified default. It matches NBReq's Rust
  1.85 floor and uses the platform trust decision on Windows while using the system CA bundle with
  WebPKI on Linux. Deterministic fixtures use a private generated test root instead of modifying an
  operating-system store.
- Pin `hickory-proto` 0.25.2 with default features disabled and only `std`. NBReq uses its DNS wire
  message/name/record parsing, not Hickory's Tokio resolver or socket runtime. The newer 0.26 line
  requires Rust 1.88 and is therefore outside the current MSRV.
- `getrandom` 0.3.4 seeds the private resolver transaction sequence from the operating-system
  random source. It was already present in the selected dependency graph; making it direct avoids
  relying on a transitive crate and lets resolver construction fail closed if randomness is
  unavailable.
- All four are private `native` feature dependencies. Their types do not enter the public API.

## First proving slice

1. Add a deterministic local UDP DNS fixture and an injected nameserver configuration.
2. Prove A/AAAA recognition, wrong transaction/source rejection, malformed/truncated/error replies,
   bounded retry/deadline behavior, cancellation before and after send, and prompt service join.
3. Connect the resolved literal address through the accepted WP7 reactor while preserving the URL
   hostname for Host and TLS identity.
4. Add generated local TLS fixtures for verified success, wrong host, unknown root, expiry, alert,
   stalled handshake cancellation, fragmented encrypted traffic, and explicit chain-and-hostname
   bypass parity.
5. Run the exact-source Windows and Ubuntu 20.04/Rust 1.85 suites, repeated cancellation/shutdown,
   thread/socket leak checks, strict clippy, formatting, and notices before accepting WP8.

## Windows progress

The first A-record/injected-nameserver slice is implemented. One connected ephemeral UDP socket
accepts replies only from the configured fixture server. A random starting transaction ID plus the
question name and type are checked before a response can complete a request. Hickory encodes and
parses the packet; NBReq owns retry clocks, bounds, commands, cancellation, and the thread.

Focused tests prove direct resolver wake/result/join, cancellation without late delivery, a public
request cancelled while the fixture holds its DNS reply, and an end-to-end hostname request that
connects to the returned IPv4 address while retaining the original hostname and port in the HTTP
Host field.

Rustls is now driven over that same real socket path. The TLS state is sans-I/O and remains on the
native backend owner; it emits bounded encrypted flights and accepts bounded encrypted reactor
events. The HTTP request is not encrypted or queued until the handshake succeeds. Connect timeout
therefore remains live through DNS, TCP, and TLS, while encrypted and plaintext progress refreshes
inactivity. Only `http/1.1` is offered through ALPN.

Generated-root fixtures prove verified HTTPS success, wrong-host rejection as
`TransportStage::Tls`, the explicit chain-and-hostname bypass, and cancellation after the peer has
observed ClientHello. The bypass skips certificate-chain and hostname acceptance only; rustls still
cryptographically verifies the server's TLS 1.2/1.3 handshake signature. A separate sans-I/O test
proves request encryption and response decryption, and the platform verifier configuration builds
with an explicit Ring provider rather than process-global provider state. The complete Windows
native suite passes 79 unit, 4 public-contract, and 2 doctests at this point, with strict clippy and
formatting. This is progress evidence, not WP8 acceptance.

## Deliberate remainder

- System resolver configuration discovery, bounded positive/negative caching, TTL clamps, IPv4/IPv6
  ordering, TCP fallback for truncated DNS replies, and Happy Eyeballs follow the injected-fixture
  vertical slice. A fixture-only resolver is not a production DNS claim.
- The current first slice deliberately accepts direct A answers for the original name only. AAAA,
  CNAME-chain validation, randomized IDs beyond the random sequence seed, DNS-over-TCP fallback,
  richer server rotation, and response-code fixtures remain before the resolver can be called
  consumer-ready.
- Platform verification may perform operating-system certificate work synchronously inside the TLS
  state machine. WP8 must measure cancellation/shutdown behavior on supported Windows and Linux
  targets and must not claim prompt cancellation around an unbounded verifier call without proof.
- Native unknown-root, expired-certificate, TLS-alert, abrupt encrypted EOF, large certificate
  chain, and encrypted response-limit fixtures remain. Verified system/platform trust has only a
  configuration smoke test so far; generated-root success does not prove the supported OS stores.
- Connection reuse, redirects, streaming/backpressure, proxy policy, and native default selection
  remain WP9/WP10.
- Parser and DNS wire fuzz targets plus a checked-in seed corpus remain required before native
  release. Deterministic fragmentation/property tests alone do not close fuzzing.
