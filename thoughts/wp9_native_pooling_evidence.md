# WP9 native pooling, redirects, and streaming evidence

Status: **WP9.1/WP9.2 pooling accepted on Windows and Ubuntu 20.04.** WP8's DNS/TLS owner is
accepted. WP9.0 boundary hardening is checkpointed at `a39adb1`; the conservative connection-reuse
slice below does not change ordinary `Engine::new` or any GDS backend selection.

## Frozen pool ownership contract

- One native Engine owns every connecting, leased, idle, and closed socket state. A Client, waiter,
  callback, request, resolver result, or TLS verifier never owns or outlives a pooled socket.
- A lease key is normalized scheme, original DNS/Host identity, effective port, and TLS verification
  policy within the Engine's immutable TLS configuration. There is no cross-host, certificate,
  address, or protocol coalescing.
- One socket has at most one request lease. HTTP pipelining and transparent replay are forbidden.
- Return requires a fully drained request write, an unambiguously framed persistent response, exact
  consumption of all plaintext, a clean TLS state, and no close policy or peer close.
- Cancellation, timeout, transport/HTTP/limit failure, dirty TLS EOF, close-delimited framing,
  trailing or unsolicited bytes, request/response close policy, idle FIN/error/expiry, and Engine
  shutdown all destroy the socket.

## First reuse slice

The response decoder now reports both the exact consumed byte count and a conservative HTTP/1.0 or
HTTP/1.1 persistence decision. Bytes following a complete response are not discarded: the transfer
fails at the HTTP stage and its socket closes. Request serialization no longer generates
`Connection: close`; an explicit request policy other than a clean keep-alive remains non-reusable.

The native owner retains clean idle entries in maps keyed by the contract above. Initial private
bounds are 32 idle sockets globally, 4 per origin/policy key, and a 30-second idle lifetime. Leasing
first performs a non-consuming, nonblocking one-byte peek. Readable bytes, FIN, or socket error
destroy the entry before any new request byte is queued; only a quiet result resets reactor receive
accounting, queue bounds, and deadlines. Cleartext writes then reuse the socket; rustls state stays
on the owner and encrypts the next buffered request without another handshake. A close racing after
the quiet probe fails that request and is never transparently replayed, regardless of method.

Connecting, leased, and idle sockets now share separate private active bounds of 32 globally and 8
per origin/policy key. A fresh connection reserves capacity before DNS or socket creation and every
terminal path releases it exactly once; an idle socket keeps its reservation until reuse or close.
When capacity is exhausted, accepted requests wait in an Engine-owned queue already bounded by the
public inflight limit. The owner starts the oldest eligible request: a head request blocked only by
its per-origin cap does not prevent an older-capacity-free origin from using a global slot, but keeps
its place and starts once that origin becomes eligible. Queue time remains part of the original
total and inactivity deadlines. There is still no replay after a leased socket fails.

Pooling exposed one real lifecycle assumption: the generic spawned loop previously stopped polling
when its public active-request map became empty. A backend can now declare owner-side idle work, so
the 50 ms native poll continues until FIN, error, expiry, reuse, or shutdown removes the last idle
entry. This is not represented by a fake request and does not weaken request terminal arbitration.

## Current proof

- A cleartext fixture accepts once and serves two sequential public blocking requests.
- A generated-root HTTPS fixture accepts and handshakes once, then serves two sequential requests on
  the same rustls connection.
- HTTP/1.1 default persistence, `Connection: close`, HTTP/1.0 default-close and explicit keep-alive,
  unknown connection tokens, and exact trailing-byte position are parser-tested.
- A response close policy forces the next request onto a second accepted socket.
- A server-side FIN after a nominally persistent response is detected while idle; 25 repetitions
  prove the entry is evicted and the next request opens a replacement socket.
- A manual Engine completes and parks one response, then the server injects an entire forged response
  while no background poll can run. Ten repetitions prove lease-time peek destroys the poisoned
  socket before request send and the real second response arrives on a replacement connection.
- A second request leases the first request's clean socket, is cancelled after the server observes
  it, and closes that connection; 10 repetitions prove a third request uses a replacement socket.
- With private limits reduced to two globally and one per origin, a stalled first-origin request
  holds its slot, a second same-origin request waits, and a later eligible second-origin request
  completes. Releasing the first request then admits the queued same-origin request on the clean
  reused socket.
- With both active limits reduced to one, a second accepted request expires from the acquisition
  queue under its original 100 ms total deadline without opening a second socket.
- Manual mode completes two sequential requests on one accepted cleartext socket.
- Malformed and declared-oversize responses on a reused cleartext connection fail with their
  portable HTTP/limit classifications, destroy that connection, and let only a later explicit
  request open a clean replacement.
- A corrupt encrypted record after a clean reused TLS request fails at the established receive
  stage, destroys the rustls connection, and lets a later explicit request create a fresh TLS
  session. Host identity and verified-versus-bypass TLS policy each force separate live sockets even
  when all names resolve to the same address and port.
- A reused connection closed after the peer observed the second request fails at Receive. A 200 ms
  accept probe proves NBReq does not replay it—even though it was GET—and only request three opens a
  replacement. Ten repetitions pass.
- Synthetic owner time advances a parked entry to its 30-second expiry without wall-clock waiting;
  the socket closes and both idle and active reservations return to zero. A separate spawned
  shutdown fixture closes one parked and one leased socket, cancels the leased waiter, and joins.
- Existing cancellation, timeout, TLS dirty-EOF, framing, and failure paths remain destructive and
  never call the pool-return path.

## Ubuntu 20.04 acceptance

The final exact source is commit `97d3c13`, archive size 375,527 bytes, SHA-256
`115A82081745AFE50D17ACE061FBDDDFBD117BA03759CAD028E0F9CE6632BAF5`. The copied archive matched
before extraction into a fresh directory on `gds-srv-test2`, Ubuntu 20.04.6 x86-64 with Rust,
Cargo, and Clippy 1.85.0.

The exact tree passes 116 native unit tests, 4 shared adversarial tests, 4 public-contract tests,
and 2 compile-fail doctests. The dependency-free default passes 41 unit, 4 contract, and 2
doctests. Warning-denied all-target native/test-support Clippy, formatting, and offline all-feature
compilation—including the retained curl pilot—all pass.

The complete native HTTP pool module followed by the DNS/TLS module then passes 25 consecutive
iterations: 50 module runs covering reuse, lease probing, caps/fairness, contamination,
no-transparent-replay, idle expiry, mixed shutdown, DNS, TLS, and joined lifecycle. No NBReq,
adversarial, or contract test process survives. The first exact archive usefully exposed only a
Rust-1.85 Clippy spelling difference in the acquisition deadline scan; commit `97d3c13` applies the
semantic no-op and reruns every gate from a fresh extraction.

WP9.1 ownership/contamination and WP9.2 conservative reuse are accepted. WP9.3 redirects may begin.

## Accepted boundary and later work

- Retain the synchronous platform-verifier head-of-line limitation and measure it with pooled
  concurrency in WP9.5. Pooling reduces handshake frequency but does not make an OS callback
  interruptible.
- Redirects remain WP9.3. Incremental upload/download and removal of the one-shot HTTPS body ceiling
  remain WP9.4. Public limits, metrics, fuzzing, pressure runs, and supported-platform evidence remain
  WP9.5/WP10.
