# WP9 native pooling, redirects, and streaming evidence

Status: **WP9.1/WP9.2 pooling, WP9.3 redirects, and WP9.4 streaming accepted on Windows and Ubuntu
20.04.** WP8's DNS/TLS owner is accepted. WP9.0 boundary hardening is checkpointed at `a39adb1`;
none of the private native slices below changes ordinary `Engine::new` or any GDS backend
selection.

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

## WP9.4g per-slot socket read allowance — Windows slice

`NativeReactor` now owns an optional read allowance on every live connection. `None` preserves the
existing buffered/unbounded-within-wire-limit behaviour. `Some(n)` caps the sum of all socket reads
performed for that slot in one or more readiness passes until the owner explicitly replenishes it;
each successful read decrements the allowance. At zero the reactor removes readable interest rather
than repeatedly waking and spinning on a full consumer queue. Writable progress and deadlines remain
independent.

A deterministic socket fixture has the peer write ten already-available bytes. The owner admits
exactly 3, observes no further Data while paused, then reopens exactly 2 and 5 bytes. This proves the
cap applies to the complete readiness batch rather than merely each 16 KiB syscall buffer. Reuse
preparation clears transfer-local allowance so a stale paused stream cannot contaminate a later
buffered lease. An explicit one-byte reopening observes peer FIN after all data was consumed.

That EOF reopening is a required owner rule: zero allowance intentionally suppresses FIN because
FIN is delivered as readability. A backpressured close-delimited response remains paused until the
reader creates capacity; fixed/chunked completion is framing-driven and needs no sentinel FIN. The
future stream owner must also reopen for EOF when close-delimited framing is active. TLS record
allowance and retained plaintext remain the next layer and are not claimed by this slice.

The Windows gate passes 150 native/test-support unit tests, 4 adversarial tests, 4 public-contract
tests, and 6 doctests; 62 default unit tests, warning-denied all-feature clippy, and formatting pass.
Native stream capability remains off.

## WP9.4h bounded TLS streaming window — Windows slice

The rustls owner now has a streaming-only receive path distinct from the buffered batch path. A
streaming socket may admit at most one 18 KiB encrypted window before returning to the HTTP owner.
That is a conservative maximum-record-sized memory allowance, not a second TLS parser and not a
claim that the window contains exactly one wire record. Any application plaintext produced from
the window remains inside `NativeTls` until the HTTP decoder explicitly consumes it.

While retained plaintext exists, the next socket read allowance is exactly zero. Once it drains,
the owner may replace the allowance with one fresh 18 KiB window; a full response queue likewise
keeps it at zero after handshake. A 64 KiB generated-rustls fixture feeds ciphertext only in those
windows, drains retained plaintext through repeated 100-byte consumer holes, proves byte-for-byte
delivery, and rejects input one byte beyond the advertised allowance before rustls sees it.

The isolated streaming decoder also now proves the close-delimited paused-FIN rule explicitly: it
fills a three-byte reader queue, retains the remaining two response bytes outside the decoder,
drains and delivers those bytes after capacity reopens, and only then lets EOF complete the reader.
This is the policy the socket owner must apply when FIN arrives while readable interest is paused.
Fixed and chunked bodies remain framing-complete without waiting for FIN.

The Windows gate passes 152 native/test-support unit tests, 4 adversarial tests, 4 public-contract
tests, and 6 doctests; 62 default unit tests, warning-denied all-feature clippy, documentation, and
formatting pass. Native stream capability remains off. The next slice connects this TLS retention,
the socket allowance, and the decoder through the real native request lifecycle.

## WP9.4i buffered-upload response streaming — Windows slice

The native backend now advertises its first honest public streaming subset. A `StreamRequest` with
a replayable buffered body (including `StreamRequest::from(Request)`) carries one unique
`ResponseSink` through DNS, TCP, rustls, HTTP framing, redirects, pooling, cancellation, and
consuming shutdown, and returns one direct `ResponseReader`. It never constructs or adapts a
buffered `Completion`. Fixed and chunked `UploadBody` producers remain deliberately unsupported:
the backend closes the producer and fails the reader with `Unsupported` before any wire claim.

Streaming pending and active states retain the sink on every construction and resolver failure so
the specific DNS/connect/TLS/HTTP/limit terminal wins before producer Drop. The live owner replaces
each socket allowance from the reader's current hole, retains unconsumed cleartext beside the
decoder and unconsumed TLS plaintext inside rustls, and only replenishes after the reader wakes it.
Consumer backpressure suppresses inactivity while total time continues. Final no-body heads reach
EOF immediately; close-delimited EOF waits for retained bytes; a dirty TLS EOF may complete an
already exactly framed response but always contaminates the connection, while an incomplete or
close-delimited response fails as Receive.

Integration found two real same-poll bugs. First, a bounded reactor read could emit multiple Data
events, allowing the first TLS window to retain plaintext before the second encrypted fragment was
delivered. Bounded reads are now aggregated into one event without changing the buffered path.
Second, PeerClosed could follow Data before a slow reader drained retained bytes. The stream owner
now remembers peer closure and applies EOF only after those bytes are consumed. Public spawned and
manual cleartext/HTTPS fixtures cover tiny response windows, 64 KiB multi-record TLS, redirect-body
discard, immediate no-body EOF, decode-stage preservation, DNS/TLS cancellation, stalled-socket
cancel and shutdown, inactivity pause, and total-timeout enforcement.

The Windows gate passes 161 native/test-support unit tests, 4 adversarial tests, 4 public-contract
tests, and 6 doctests; 62 default unit tests, warning-denied all-feature clippy, documentation, and
formatting pass. The next slice pumps fixed-length and chunked `UploadBody` data incrementally; it
does not widen the buffered family or the ordinary `Engine::new` backend choice.

## WP9.4j streamed upload pump — Windows slice

The native owner now consumes the unique `UploadBody` directly. Fixed bodies generate an exact
`Content-Length`; chunked bodies generate one HTTP/1.1 chunk per producer chunk plus the terminal
zero chunk. Request heads, producer data, and the final marker enter the existing bounded reactor
queue incrementally. On TLS connections each producer chunk becomes rustls plaintext only after
the preceding plaintext and ciphertext have drained, preserving the accepted 512 KiB encrypted
queue bound without buffering the complete upload or making it replayable.

`UploadSender::push(Vec)` is the spawned-mode blocking convenience. It admits buffers larger than
the transfer window in pieces, waits on the existing producer condition variable, and returns only
the unaccepted suffix if an early final response, cancellation, failure, or Engine stop closes the
receiver. Before submission it returns `NotSubmitted`; on a manual Engine it returns `WrongMode`
and never drives the owner. `try_push` remains the passive all-or-nothing operation for manual and
spawned callers. Fixed length mismatch and producer abandonment fail the reader at Send.

A final HTTP response wins over an unfinished upload: queued producer bytes are discarded, blocked
senders wake Closed, the 4xx/5xx head/body remains a normal reader result, and the connection is
never pooled unless the complete request was already proven drained. Live uploads never follow a
redirect. An impossible redirect-sink transfer now also contaminates the socket explicitly rather
than failing the reader and parking uncertain state.

Windows fixtures prove exact fixed and chunked cleartext wire bytes, generated-header limits,
64 KiB fixed HTTPS upload through a 4 KiB window using blocking push, manual prequeue/try-push and
owner drive, a completed upload returning one clean socket to the pool, a 303 returned unfollowed,
an 8 MiB producer interrupted by a backpressured 413 response, abandonment, fixed length mismatch,
individual cancellation, and consuming shutdown. The inactivity test now waits
for an observed full response queue before its clock assertion instead of assuming a sleep filled
it. The native gate passes 172 unit tests, 4 adversarial tests, 4 public-contract tests, and 6
doctests; the default gate passes 63 unit tests, 4 public-contract tests, and 6 doctests. Strict
all-feature clippy, documentation, and formatting pass. The combined curl/native test run retains
three pre-existing vendored-Schannel fixture failures before ClientHello on this host; isolated
reruns reproduce them, and no curl source changed in this slice.

## WP9.4 Ubuntu 20.04 acceptance

Exact commit `d3d2809` was packaged as a 427,945-byte source archive with SHA-256
`A700B793B9AB7B91B69F5CB56C2A61C3C4629103554CD16BB92A52BF3DBC6FA4`, copied to the Ubuntu
host, verified before extraction, and built in a fresh directory using rustc/cargo 1.85.0. The
default gate passes 63 unit tests, 4 public-contract tests, and 6 doctests. The native/test-support
gate passes 172 unit tests, 4 adversarial tests, 4 public-contract tests, and 6 doctests. Strict
all-target native/test-support clippy, formatting, and offline all-feature compilation pass.

The first exact archive (`f749df5`) usefully rejected one Windows-shaped cancellation fixture. On
Linux the peer received 1,024 upload bytes already accepted by the kernel before it observed the
socket close. The producer still woke `Closed` with its unaccepted suffix and the Engine joined;
the transport did not continue pumping after cancellation. Commit `d3d2809` corrected the fixture
to drain bounded in-flight socket bytes and still require the close, rather than claiming that
cancellation can recall an already completed write. The corrected test passes 50 consecutive
Windows repetitions and the full local gate before packaging. On Ubuntu, 25 consecutive pairs of
the cancellation/shutdown and streamed-upload test filters pass, followed by a check that finds no
surviving proof process.

WP9.4 is accepted on the declared Windows and native Ubuntu targets. This does not select native
through ordinary `Engine::new`, change the curl pilot or ureq rollback, or claim WP9.5 production
limits, metrics, fuzzing, pressure, benchmark, Wine-native, or public-backend readiness.

## Accepted boundary and later work

- Retain the synchronous platform-verifier head-of-line limitation and measure it with pooled
  concurrency in WP9.5. Pooling reduces handshake frequency but does not make an OS callback
  interruptible.
- Fixed/chunked `UploadBody` pumping, blocking producer wakeups, buffered-upload response
  streaming, direct reader delivery, and bounded cleartext/TLS backpressure are accepted on
  Windows and exact-source Ubuntu 20.04. The one-shot HTTPS body ceiling was removed by WP9.4a.
  Public limits, metrics, fuzzing, pressure runs, and supported-platform evidence remain
  WP9.5/WP10.
