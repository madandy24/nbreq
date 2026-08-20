# WP8 native DNS and TLS evidence

Status: **accepted on 2026-08-20 for the private native backend.** WP7 supplies the accepted socket,
deadline, cancellation, and HTTP/1.1 owner. This is native DNS/TLS acceptance, not yet a
production-native or GDS-selection claim; WP9–WP10 retain pooling, redirects, pressure/parity, and
default-selection work.

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
- Pin target-specific `ipconfig` 0.3.4 on Windows and `resolv-conf` 0.7.6 on Unix. They read only
  platform DNS configuration; NBReq does not adopt a resolver runtime from either. Both support an
  older Rust floor than NBReq and use the customary MIT/Apache dual grant.
- All six are private `native` feature dependencies. Their types do not enter the public API.

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

The injected-nameserver slice is implemented. One connected ephemeral UDP socket
accepts replies only from the configured fixture server. A random starting transaction ID plus the
question name and type are checked before a response can complete a request. Hickory encodes and
parses the packet; NBReq owns retry clocks, bounds, commands, cancellation, and the thread.

Focused tests prove direct resolver wake/result/join, cancellation without late delivery, a public
request cancelled while the fixture holds its DNS reply, bounded CNAME following, serial A-to-AAAA
fallback, and rejection of wrong questions. A truncated UDP response moves the same query onto a
nonblocking, poll-owned DNS-over-TCP connection with bounded length framing. Fragmented TCP length,
message delivery, and cancel-to-peer-close under 500 ms are proven. An end-to-end hostname request
connects to the returned IPv4 address while retaining the original hostname and port in the HTTP
Host field.

Rustls is now driven over that same real socket path. The TLS state is sans-I/O and remains on the
native backend owner; it emits bounded encrypted flights and accepts bounded encrypted reactor
events. The HTTP request is not encrypted or queued until the handshake succeeds. Connect timeout
therefore remains live through DNS, TCP, and TLS, while encrypted and plaintext progress refreshes
inactivity. Only `http/1.1` is offered through ALPN.

Generated-root fixtures prove verified HTTPS success, wrong-host, unknown-root, and expired-certificate
rejection as `TransportStage::Tls`, the explicit chain-and-hostname bypass, and cancellation after
the peer has observed ClientHello. The bypass skips certificate-chain and hostname acceptance only;
rustls still cryptographically verifies the server's TLS 1.2/1.3 handshake signature. A separate
sans-I/O test proves request encryption and response decryption, and the platform verifier
configuration builds with an explicit Ring provider rather than process-global provider state.

The TLS abuse seam is now explicit. A peer alert during handshake is a `Tls`-stage transport
failure. Incoming handshake bytes and outgoing rustls flights each have a fixed 512 KiB private
budget checked before owned growth. A real encrypted chunked response proves the portable
plaintext response-body limit still wins after decryption. A real server that encrypts a
close-delimited response and then drops TCP without `close_notify` is rejected at `Receive` rather
than being accepted as authenticated EOF; cleartext HTTP retains its ordinary FIN-delimited rule.
The first complete run of these fixtures exposed a parallel-test-only Windows port-reservation
race in the DNS-over-TCP laboratory. The fixture now reserves the shared UDP/TCP numeric port in
the Windows-compatible order and retries collisions.

The first live host-DNS/platform-store request then exposed a real batching bug: one reactor event
can contain more encrypted records than rustls accepts into its internal input buffer in one
`read_tls` call. NBReq now alternates bounded input, packet processing, plaintext draining, and
outbound collection until the complete reactor event is consumed. A 128 KiB many-record unit
fixture guards that path. The opt-in `native_platform_https` proving executable uses only the
private system-DNS/platform-TLS constructor; it does not change ordinary backend selection. On the
Windows development host it completes `https://example.com/` with verified platform trust and HTTP
200 while printing only status and body length. The complete Windows native suite passes 96 unit,
4 public-contract, and 2 doctests at this point, with strict clippy and formatting. This is progress
evidence, not WP8 acceptance.

Supported-platform configuration discovery is also wired behind private test support. Windows reads
DNS servers only from operational adapters, retains IPv6 scope IDs, ranks them by the applicable
adapter metric, removes duplicates, and fails closed if none remain. Unix parses `/etc/resolv.conf`,
clamps retry settings, and rejects scoped link-local IPv6 servers rather than silently discarding an
interface name. A construction/shutdown fixture initially exposed an unreachable IPv6 server on an
otherwise operational Windows adapter; construction now tries the ranked list until the kernel
accepts a connected UDP route. The fixture proves that selected server, platform TLS configuration,
resolver thread, and Engine can be created and joined together. A separate two-server fixture proves
that a kernel-reachable but silent server exhausts its bounded attempt, the owner replaces its
registered UDP socket, and the same query completes through the next ranked server.

The resolver owner also holds a fixed 256-entry cache. Positive answers retain the DNS TTL up to
one hour; authoritative NXDOMAIN/no-data results are cached only when the response supplies an SOA
lifetime, capped at five minutes. Zero-TTL and non-authoritative failures are never cached. Expiry is
checked before delivery; insertion evicts the least-recently-used entry before crossing the bound.
Deterministic one-query/two-request fixtures prove positive and authoritative-negative hits, and a
direct clock fixture proves zero-TTL, expiry, clamp, and capacity behavior. A CNAME that needs a
second wire query remains uncached until its full-chain TTL can be carried forward.

## Ubuntu 20.04 acceptance

The final exact source is commit `c1f123e`, archive SHA-256
`777E1123EA2CA95A8D46CF4DDD0D6E2AC8CD2BBC6B734DA00781C91E657C70E2`. It was copied to
`gds-srv-test2`, verified before extraction, and built in a fresh directory on Ubuntu 20.04.6
x86-64 with Rust/Cargo/Clippy 1.85.0.

The final tree passes 96 unit tests, 4 public-contract tests, and 2 compile-fail doctests. Strict
clippy over all targets with `native,test-support` and warnings denied passes; formatting and the
all-feature compile pass. The reusable proving executable then uses that host's `/etc/resolv.conf`
discovery and platform trust to complete `https://example.com/` with HTTP 200 and a 559-byte body,
printing no response content.

The exact tree subsequently passes 25 complete repetitions of the native DNS module followed by
the native TLS module. This includes UDP/TCP resolution and cancellation, failover, cache,
generated certificate policy, TLS alert/flight/multi-record/dirty-EOF behavior, HTTPS limits, and
ClientHello-barrier cancellation. No NBReq test process remains afterward.

The exact-source path found two useful defects rather than being ceremonial. The first archive's
functional suite passed, but Unix warning-denied lint caught a Windows-only DNS timeout constant
without a target guard. A later live platform-trust request exposed the multi-record rustls input
batching bug described above. Both were corrected and the final archive reran every gate from a
fresh extraction.

## Accepted boundary and deliberate remainder

- Search-suffix behavior, full CNAME-chain cache lifetime propagation, configurable cache policy,
  and Happy Eyeballs remain after basic system discovery, bounded caching, silent-server failover,
  and TCP truncation fallback. This is not yet a production DNS claim.
- The current slice uses serial A then AAAA lookup and bounded CNAME following. Happy Eyeballs,
  randomized IDs beyond the random sequence seed, adaptive server health, and response-code
  fixtures remain before the resolver can be called consumer-ready. Current failover is a bounded
  ordered walk, not a latency-ranking pool.
- Platform verification may perform operating-system certificate work synchronously inside the TLS
  state machine. Network stalls before and after verification remain cancellable through the owner;
  the verifier callback itself cannot be pre-empted until it returns. Current Windows and Linux
  platform-trust requests complete normally, but NBReq does not claim a hard cancellation latency
  inside an arbitrary operating-system verifier callback.
- A synthetically oversized peer handshake proves the hard wire ceiling. A valid generated
  certificate chain near that ceiling remains useful stress evidence, but it cannot weaken or
  replace the bound. Windows platform trust now has one live public HTTPS proof; the exact Ubuntu
  platform-store run remains. Generated-root success alone does not prove a supported OS store.
- Connection reuse, redirects, streaming/backpressure, proxy policy, and native default selection
  remain WP9/WP10.
- Parser and DNS wire fuzz targets plus a checked-in seed corpus remain required before native
  release. Deterministic fragmentation/property tests alone do not close fuzzing.

WP8's acceptance contract is met: valid verified HTTPS succeeds; wrong host, expired certificate,
unknown root, alert, dirty EOF, and interrupted handshake fail in the intended portable category;
request cancellation closes resolver/TLS network work promptly; and shutdown leaves no resolver or
TLS worker alive. WP9 may build pooling, redirects, streaming/backpressure, and production
connection policy on this owner without reopening the DNS/TLS ownership seam.
