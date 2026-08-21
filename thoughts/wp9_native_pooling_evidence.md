# WP9 native pooling, redirects, and streaming evidence

Status: **WP9.1/WP9.2 pooling and WP9.3 redirects accepted on Windows and Ubuntu 20.04.** WP8's
DNS/TLS owner is accepted. WP9.0 boundary hardening is checkpointed at `a39adb1`; none of the
private native slices below changes ordinary `Engine::new` or any GDS backend selection.

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

WP9.1 ownership/contamination and WP9.2 conservative reuse are accepted. The following section
records the accepted WP9.3 redirect slice.

## WP9.3 conservative redirects — accepted

Redirect policy is now one backend-neutral function shared by the retained curl pilot and native
owner. A native hop never creates a second public request or terminal identity: it retires the
completed connection according to the accepted pool rules, keeps the same `RequestId`, preserves
the original absolute total deadline, resets only per-hop connect/inactivity clocks, and reacquires
through the same active caps and oldest-eligible queue. Cancellation therefore finds and closes the
current DNS, connect, TLS, queued, or HTTP hop without a redirect-specific side channel.

The accepted policy is deliberately conservative:

- 301/302 follow only GET and HEAD; buffered POST and extension methods return the redirect response
  rather than silently rewriting or replaying it.
- 303 becomes GET without a body, except HEAD remains HEAD. 307/308 preserve the buffered method and
  body. WP9.4 must make replayability explicit before adding streaming request bodies.
- Zero redirect limit returns the first redirect response and does not inspect `Location`. A missing
  `Location` also returns the response. Duplicate fields, non-UTF-8 values, unresolvable or
  non-HTTP(S) targets, and hop-limit exhaustion fail with `ErrorKind::Redirect`.
- Relative and network/absolute references are resolved with pinned pure-Rust `url` 2.5.8. An
  HTTPS-to-HTTP downgrade is rejected. Crossing scheme/host/effective-port strips Authorization,
  Proxy-Authorization, Cookie, and caller Host while retaining unrelated headers.
- A clean same-origin redirect may reuse its just-completed connection; cross-origin policy and pool
  keys still require the appropriate destination lease. There is no transparent transport replay.

Deterministic Windows fixtures prove 301/302 POST non-follow, 303 body drop, 307/308 buffered-body
preservation, HEAD-on-303 policy, relative path/query resolution, same-origin authorization and
socket reuse, cross-origin credential/Host stripping, missing/duplicate/invalid/unsupported
destinations, exact hop exhaustion, HTTPS downgrade refusal, cancellation after the target hop has
received its request, and a total timeout that expires against original acceptance rather than
being reset after the redirect. The broad native gate passes 122 unit, 4 adversarial, 4 contract,
and 2 doctests. Default tests, all-feature compilation, warning-denied all-feature clippy, formatting,
and the existing curl redirect fixture also pass.

The exact accepted source is commit `bba1d24`, archive size 377,980 bytes, SHA-256
`C1EF28D33AD1F0E45CD134E7055333E0F87697D77482799FF11EA85BEC535B05`. The copied archive matched
before extraction into a fresh directory on `gds-srv-test2`, Ubuntu 20.04.6 x86-64 with Rust,
Cargo, and Clippy 1.85.0. It passes the same 122 native unit, 4 adversarial, 4 contract, and 2
doctests; the 42-test default suite, warning-denied native/test-support clippy, formatting, and
offline all-feature compilation also pass. Twenty-five consecutive redirect-matrix repetitions
then complete with no surviving NBReq, adversarial, or contract process. WP9.3 is accepted and
WP9.4 may begin.

## WP9.4a buffered TLS pump — Windows slice

The first streaming seam removes the temporary native HTTPS request ceiling without freezing a
public streaming API. `NativeTls` now retains a cursor over the already accepted buffered request,
feeds rustls plaintext in 16 KiB pieces, and emits only as much ciphertext as the reactor's current
queue capacity permits. The reactor TLS queue is a constant 512 KiB private bound independent of
request size. Write-progress/drained readiness refills that queue; a request is marked fully written
only after all plaintext has been encrypted and the final ciphertext has drained. The same pump is
used for a fresh handshake and a reused rustls connection.

A sans-I/O test drives more than 1 MiB of request plaintext through an artificial 32 KiB ciphertext
budget and proves every emitted batch stays within that budget. A generated-root socket fixture
uploads more than the old 512 KiB ceiling, receives a response, then reuses the same TLS connection
for a second request. Another fixture cancels after the server has received 128 KiB of an 8 MiB
upload and proves canonical `Cancelled`, peer close, and Engine shutdown within the 500 ms gate. A
server that returns HTTP 413 while the 8 MiB upload is still in progress remains a completed HTTP
response rather than overflowing or corrupting the TLS send queue. Twenty-five targeted repetitions
pass. The full Windows native gate is 125 unit, 4 adversarial, 4
contract, and 2 doctests; default tests, all-feature compilation, warning-denied all-feature clippy,
and formatting pass.

This is deliberately not yet the whole WP9.4 claim. `Request` and `Response` remain buffered public
values, cleartext serialization can still queue up to the configured request limit, and one reactor
readiness pass can still drain multiple response chunks into an owned event batch. True streaming
requires a public producer/consumer and replayability contract; that contract must be reviewed
before implementation rather than inferred from the internal TLS cursor.

## WP9.4b public contract and ownership primitives — Windows slice

The reviewed streaming contract is now frozen in the product specification and delivery plan.
Buffered `Request` / `Response` / `Completion` remain untouched. `StreamRequest` is a complete
builder with replayable `.body(Vec)` and unique `.body_stream(UploadBody)` modes; selecting both is
a build error, while `From<Request>` remains convenience sugar. Every future stream submission has
one `ResponseReader` terminal consumer and no `PendingRequest`, streaming `Completion`, callback, or
second body waiter.

The first implementation slice adds the unique fixed-length and chunked upload pairs without
claiming transport submission. `UploadBody` is consumed by one StreamRequest while its caller keeps
one `UploadSender`; both are `Send`, deliberately `!Sync`, and not `Clone`. The initial
`try_push(Vec)` is all-or-nothing and returns the unchanged buffer for a full queue, an impossible
oversize chunk, a fixed-length overflow, or a closed body. Fixed finish enforces the exact declared
length, chunked finish is explicit, finish consumes the producer, drop-before-finish poisons later
construction, and body drop closes the sender. Request construction rejects mixed body modes,
GET/HEAD stream uploads, caller framing headers, `Expect`, zero queue capacity, abandoned senders,
and length-mismatched finish.

The public submission/reader path, blocking producer, Engine aggregate byte budget, total-limit
binding, cleartext/TLS wire pump, response backpressure, early response, cancellation, manual mode,
and curl `Unsupported` result remain the next slices. Pre-submission queue bytes are caller-owned;
Engine accounting begins only when submission accepts and binds the channel.

## WP9.4c response-reader and Engine-facing channel halves — Windows slice

The unique public `ResponseReader`, immutable `ResponseHead`, `StreamRead`, and `StreamError` state
machine now exist without adding `submit_stream`. The reader caches only the final head, drains one
bounded queue, owns the sole terminal observation path, and exposes a cloneable cancellation-only
RequestHandle. Spawned `wait_head`, `read`, and untouched-reader `collect` block only on the response
condition variable. Manual mode returns `WrongMode` for those calls while `try_head` and `try_read`
remain passive and never drive. `collect` returns an ordinary `Response`, but fails after any body
byte was already delivered.

No-body final heads establish EOF immediately. The final byte of an ordinary body also establishes
EOF without requiring a sentinel read, so reader drop is harmless in both cases. Drop before known
EOF clears unread data, wakes the transport, and cancels through RequestHandle. A full queue rejects
the complete owned response chunk; reader progress releases capacity and wakes the transport.
Failures and cancellation discard queued and reader-local remainder before surfacing terminal state.
An isolated regression closes a race where a transport failure could zero shared byte accounting
while the reader retained part of a popped chunk, which would otherwise underflow on its next read.

The upload pair now has the corresponding crate-internal owner API: acceptance-time `bind` clamps
the requested window and total bytes, rechecks sender abandonment as a Send-stage failure, installs
the Engine waker without a lost-wake gap, and rejects double binding. Native HTTP will consume only
`try_pop` states (`Chunk`, `Pending`, `Finished`, `Failed`) and `close`, never the mutex. Total-limit
refusal and Engine closure continue returning caller-owned upload chunks. A second `.body_stream`
now has its own construction diagnostic.

The full Windows native/test-support gate passes 140 unit, 4 adversarial, 4 public-contract, and 6
doctests; the 57-test default unit suite, warning-denied all-feature clippy, all-feature compilation,
and formatting also pass. This is still deliberately not a submission or wire-streaming claim.
Next, registry acceptance and native decoding must own ResponseSink directly; adapting the existing
buffered Completion would violate the frozen memory and terminal model.

## WP9.4d distinct stream admission and command seam — Windows slice

`Client::submit_stream` now enters a separate registry and reactor command lane. Buffered
`RequestState`, `PendingRequest`, callback activation, and `Completion` are unchanged. The stream
registry retains only Engine admission, a cloneable terminal control, and byte-budget permits; the
reactor receives the unique `ResponseSink` and `StreamRequest`. Cancellation by ID, `cancel_all`,
shutdown, and reactor panic commit directly into the reader state and release both permits without
waiting for a later transport poll.

Backends advertise streaming capability before acceptance. The scaffold and curl therefore return
`Unsupported` with no request identity, queue command, or admission side effect. The deterministic
held backend proves accepted stream cancellation and shutdown without manufacturing a buffered
completion. Native capability remains false until its decoder and upload pump actually own the
sink/body; the public method's existence is not a native wire-streaming claim.

Engine configuration now has a 256 KiB maximum per-direction stream queue window and a 16 MiB
aggregate reserved-byte budget by default. Acceptance reserves the full unread-response window plus
any streamed-upload window, clamps the latter, binds the producer to the Engine wake path and total
request ceiling, and releases the reservation at the exact terminal winner. A strict two-request
test proves aggregate refusal and immediate recovery after cancellation. Producer finish and
abandonment now wake a bound Engine just like `try_push`, closing the empty-queue lost-wake hole.

The Windows gate passes 144 native/test-support unit tests, 4 adversarial tests, 4 public-contract
tests, and 6 doctests; the default suite, warning-denied all-feature clippy, all-feature compilation,
and formatting pass. Curl's locally generated Schannel fixtures were flaky in one combined test run
and are not evidence for this native-only slice; no curl code changed. Next is native ownership of
the stream command, direct incremental head/body delivery, response-window read pausing/splitting,
and fixed/chunked upload pumping.

## WP9.4e stream panic-terminal ordering correction — Windows slice

Review found that the first WP9.4d panic claim was too broad. The spawned run-loop originally owned
its `ReactorCore` inside the unwind boundary. A panic therefore dropped the backend and every live
`ResponseSink` before `contain_reactor_panic` could call `fail_all`; sink Drop won with the generic
"streaming response producer ended" Internal error rather than the canonical reactor-panic error.

The reactor owner now remains outside the caught closure. Factory creation is contained as its own
phase, then the created reactor is likewise retained outside the run-loop boundary. On run-loop
panic the registry commits `NBReq reactor thread panicked` to buffered and streaming requests while
backend-held sinks are still alive; their later Drop loses the already-set terminal race. A backend
that stores the unique sink and panics during `submit_stream` proves the reader observes that exact
canonical error and shutdown keeps it observable. The existing buffered panic test remains green.
The public StreamRequest rustdoc now points to the already-landed `Client::submit_stream` method.

The corrected Windows gate passes 145 native/test-support unit tests, 4 adversarial tests, 4
public-contract tests, and 6 doctests; 62 default unit tests, strict all-feature clippy, documentation,
and formatting pass. Native wire streaming remains unsupported and is still the next slice.

## WP9.4f isolated incremental streaming decoder — Windows slice

The native HTTP module now has a distinct streaming decoder which owns `ResponseSink` directly; it
does not construct a buffered `Response` or adapt `Completion`. Native backend capability remains
off while this decoder is isolated from sockets. Informational heads stay internal. Parsing stops
at the exact final-head boundary and returns that immutable head to the owner for redirect policy
before publication or body consumption. A delivered no-body head commits EOF immediately.

Body delivery takes a snapshot of `ResponseSink::available_capacity`, consumes no more body bytes
than that current hole, and pushes one bounded chunk. It may continue across framing metadata while
full, but stops before the next fixed, close-delimited, or chunk-data byte. Tests force a three-byte
window and repeatedly open only two-byte reader holes, proving the decoder splits progress rather
than waiting for an entire input chunk or exceeding the queue. Exact consumed counts retain bytes
after message completion for contamination checks.

Redirect heads can instead be kept private. Their bodies are framing-validated and discarded under
the ordinary response total limit; on exact completion the decoder returns the same unique sink for
the next hop. A 302-to-200 fixture proves the reader sees only the final head/body. Informational,
fixed, chunked, trailer, no-body, oversize-before-publication, and terminal failure rules share the
existing parser policy and have direct reader-state assertions.

The Windows gate passes 149 native/test-support unit tests, 4 adversarial tests, 4 public-contract
tests, and 6 doctests; 62 default unit tests, warning-denied all-feature clippy, documentation, and
formatting pass. Next is socket-owner integration: per-slot read allowance must prevent a reactor
readiness batch from outrunning the reader window, while TLS may retain only its documented one-record
allowance. Native must remain capability-off until that path and cancellation are end-to-end proven.

## Accepted boundary and later work

- Retain the synchronous platform-verifier head-of-line limitation and measure it with pooled
  concurrency in WP9.5. Pooling reduces handshake frequency but does not make an OS callback
  interruptible.
- Public upload/download streaming, cleartext queue backpressure, and bounded response delivery
  remain WP9.4; the one-shot HTTPS body ceiling is removed by WP9.4a. Public limits, metrics,
  fuzzing, pressure runs, and supported-platform evidence remain WP9.5/WP10.
