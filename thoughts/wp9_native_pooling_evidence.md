# WP9 native pooling, redirects, and streaming evidence

Status: **in progress.** WP8's DNS/TLS owner is accepted. WP9.0 boundary hardening is checkpointed
at `a39adb1`; the conservative connection-reuse slice below is not yet WP9.1 acceptance and does not
change ordinary `Engine::new` or any GDS backend selection.

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
resets reactor receive accounting, queue bounds, and deadlines. Cleartext writes reuse the socket;
rustls state stays on the owner and encrypts the next buffered request without another handshake.

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
- A second request leases the first request's clean socket, is cancelled after the server observes
  it, and closes that connection; 10 repetitions prove a third request uses a replacement socket.
- Existing cancellation, timeout, TLS dirty-EOF, framing, and failure paths remain destructive and
  never call the pool-return path.

## Remainder before WP9.1 acceptance

- Add explicit contamination/replacement fixtures for malformed/oversize responses on reused
  sockets, TLS close/error after reuse, and shutdown with a mix of idle and leased connections.
- Add active global/per-origin connection caps, FIFO acquisition pressure, deadline behavior while
  queued, and anti-starvation evidence. The current constants bound idle retention only.
- Prove cross-origin and TLS-policy isolation under concurrent load, stale-idle failure without
  replay, idle expiry without waiting 30 wall-clock seconds, and manual-mode reuse.
- Retain the synchronous platform-verifier head-of-line limitation and measure it with pooled
  concurrency. Pooling reduces handshake frequency but does not make an OS callback interruptible.
- Redirects remain WP9.3. Incremental upload/download and removal of the one-shot HTTPS body ceiling
  remain WP9.4. Public limits, metrics, fuzzing, pressure runs, and supported-platform evidence remain
  WP9.5/WP10.
