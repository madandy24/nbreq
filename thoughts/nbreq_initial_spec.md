# NBReq Initial Product Specification

Status: architecture contract accepted for WP0; policy and proof details remain\
Date: 2026-08-16\
Working name: **NBReq** (name not yet accepted)\
Audience: library implementers, GDS integrators, DLL/FFI consumers, and reviewers

## 1. Product statement

NBReq is a small, self-contained HTTP client for programs that need concurrent network access, prompt cancellation, deterministic shutdown, and an ordinary synchronous or callback-oriented API without adopting an async runtime.

The library will make its real execution model explicit:

- one `Engine` owns one independent request engine and its lifecycle;
- cheap cloneable `Client` handles submit commands into an Engine but do not own or extend its lifecycle;
- programs may create multiple Engines; their mutable request state, cancellation domains, queues, connection pools, and shutdown are independent;
- requests may execute concurrently without one operating-system thread per request;
- every accepted request can be cancelled through its handle/ID or collectively through its Engine;
- blocking callers wait on the same cancellable engine rather than performing blocking socket I/O themselves;
- normal Engine shutdown fails outstanding work, wakes all waiters, drains callback delivery, and joins owned threads; timed shutdown may instead return an observable detached-callback handle after all network work has stopped;
- callbacks are queued as owned events and invoked only at a safe point after network state is committed and internal borrows/locks are released;
- no Tokio or other application async runtime is required.

The first production-capable backend will use libcurl's multi interface. A later Rust-native backend will implement the same contract using nonblocking sockets, a portable poller, TLS, and an HTTP/1.1 state machine. Backend details must not leak into the public API.

## 2. Motivation

Rust HTTP clients tend to make one of two trade-offs:

1. A blocking client is easy to embed but an in-flight call cannot normally be interrupted promptly from another thread.
2. An async client supports concurrency and cancellation but commonly imposes a runtime, executor, and shutdown model on the containing program.

Those trade-offs are especially awkward for:

- DLLs loaded by Delphi or another non-Rust host;
- GUI programs with an existing event loop;
- services whose lifecycle is not owned by Rust;
- long-poll requests that intentionally remain in flight;
- applications that must unload code without leaving detached workers behind;
- mostly synchronous programs that need several concurrent HTTP operations, not an async application architecture.

GDS supplies the first demanding consumer. Its WebRPC long poll must be cancellable during shutdown, while ordinary GET and POST callers should remain simple and may continue using a blocking interface.

## 3. Clarification about `httparse`

`httparse` is not built into the Rust standard library. It is a separate, narrowly focused Rust crate. It parses HTTP/1 request and response heads and provides chunk-size parsing, but it is not an HTTP client and does not manage sockets, TLS, redirects, body framing, connection reuse, or cancellation.

It is useful to NBReq precisely because of that narrow scope. The Rust-native backend can rely on a mature parser for security-sensitive byte recognition while retaining explicit control of the surrounding connection and request state machines. NBReq should not write a new header parser unless concrete requirements demonstrate that `httparse` is unsuitable.

References:

- <https://docs.rs/httparse/latest/httparse/>
- <https://docs.rs/polling/latest/polling/>
- <https://docs.rs/rustls/latest/rustls/>
- <https://docs.rs/curl/latest/curl/multi/struct.Multi.html>

## 4. Goals

### 4.1 Required product qualities

- **Prompt cancellation.** An in-flight request can be cancelled from another thread during queueing, DNS resolution, connect, TLS negotiation, send, response-header receive, or response-body receive.
- **Deterministic shutdown.** The owner can stop accepting requests, cancel outstanding work, wake blocking callers, stop and join all network services, and either wait for callback completion or receive an explicit handle to the sealed callback domain still draining.
- **Concurrent I/O.** One Engine can progress many network requests without allocating one permanent worker thread per request.
- **Blocking and callback APIs.** Both interfaces use the same engine and have the same timeout, cancellation, response, and error semantics.
- **Embeddability.** The library is usable from an executable, static library, Rust DLL, or a carefully written C ABI adapter without a process-global runtime.
- **Backend independence.** Curl and Rust-native engines are replaceable implementation details.
- **Bounded resource use.** Request queues, response bodies, header counts/sizes, connection pools, callback queues, and concurrency have configurable limits.
- **Good failure information.** Errors are structured, stable enough for program decisions, and preserve a diagnostic source without exposing secrets.
- **Observable operation.** Callers can obtain progress and lifecycle events and can integrate redacted logging or metrics.
- **Small conceptual surface.** Common GET/POST calls remain easy even though the implementation handles difficult lifecycle cases.

### 4.2 Initial protocol scope

The common API will support:

- `http://` and `https://` URLs;
- arbitrary HTTP methods, with first-class helpers for GET and POST;
- request headers and buffered request bodies;
- response status, headers, and buffered bodies;
- HTTP/1.1 fixed-length, chunked, no-body, and close-delimited responses;
- configurable redirects;
- Basic authentication through a safe request option or ordinary headers;
- connect timeout, inactivity/idle timeout, and total deadline;
- connection reuse where permitted;
- upload/download progress events;
- cancellation and collective Engine failure.

Streaming bodies and finer stage-specific timeouts are reserved extension points, not requirements for the buffered curl/GDS pilot or WP3. They enter the implemented common contract only in the later native/full scope with backpressure and honest backend semantics. The curl backend may support more internally, but extra curl capabilities are not portable NBReq features unless deliberately accepted.

## 5. Non-goals for the first native release

- Providing an async/await API.
- Replacing a general-purpose async runtime.
- HTTP server functionality.
- HTTP/2 or HTTP/3.
- WebSockets.
- FTP, SMTP, or other libcurl protocols.
- Automatic cookie storage.
- PAC files, NTLM, SPNEGO, or broad enterprise proxy authentication.
- Transparent response decompression unless separately accepted.
- A public C ABI in the core crate. An FFI adapter may be delivered alongside it.
- Literal absence of all platform or native cryptographic code. The full native destination aims for a self-contained artifact; the curl pilot may ship pinned runtime libraries beside GDS.

## 6. Design principles

### 6.1 Tell the truth about ownership

An `Engine` is the unique lifecycle owner of its reactor, backend, queues, active requests, connections, callback dispatcher, DNS service, and worker threads. An Engine is not cheaply cloneable. It can run on an owned worker or be manually driven by its host.

An Engine is uniquely owned and is not placed in `Arc` as part of the NBReq ownership model. `client(&self)` and `cancel_all(&self)` use interior synchronization; manual `drive(&mut self)` requires exclusive access; explicit `shutdown(self)` and `shutdown_for(self, duration)` consume the owner. If another thread needs administrative action, it signals the Engine owner rather than acquiring another Engine owner. Shutdown idempotence describes the internal transition and `Drop` fallback, not repeated calls on a value already consumed by public shutdown.

The ordinary Engine is designed to be `Send`: its one owner may move it between threads without making it cloneable or concurrently usable. NBReq does not initially promise `Engine: Sync`. Client and request-control values are designed for cross-thread use: `Client` and `RequestHandle` are `Send + Sync`, while `PendingRequest` and `DetachedCallbacks` are at least `Send`.

Users remain free to impose their own shared administrative ownership. `Engine: Send` permits a construction such as `Arc<Mutex<Option<Engine>>>`, where one participant atomically takes the Engine before consuming shutdown. A bare `Arc<Engine>` can cross threads only if Engine is also `Sync`, which is not part of the initial contract. NBReq neither depends on nor forbids such wrappers; their lock ordering, deadlock avoidance, and shutdown ownership belong to the user.

Preserving `Engine: Send` constrains stored state: spawned callbacks, request bodies/sources, commands, waiter state, and backend objects that move with an Engine must satisfy the necessary `Send` bounds. If manual same-thread consumers later require non-`Send` callbacks, that capability belongs in an explicitly local type/mode rather than weakening ordinary Engine.

A `Client` is a cheap cloneable command handle issued by an Engine through `engine.client()`. Client has no public constructor and never creates, owns, or extends an Engine. Dropping a Client does not shut down the Engine. When its Engine has stopped, a surviving Client rejects new work with `EngineStopped`.

Multiple Engines are permitted and form independent cancellation and resource domains. They may share harmless process initialization or immutable data required by a backend/TLS provider, but they do not share mutable request registries, queues, connection pools, IDs, cancellation, or shutdown.

Every Engine is constructed independently from configuration, for example `Engine::new(config)` or an equivalent builder. Engines do not spawn child/linked Engines. If immutable TLS/provider configuration is reusable, callers clone that configuration into another independent construction; unavoidable process-global backend initialization remains private implementation state rather than Engine ancestry.

During the curl pilot, enabling the crate's `curl-pilot` feature makes that same ordinary
constructor select the private spawned curl backend. It does not introduce a curl-specific public
Engine type or constructor and does not change how Clients are issued. No-feature builds retain the
non-networking lifecycle scaffold; the future native feature replaces only the private backend.

### 6.2 One terminal outcome

Every accepted request transitions exactly once to one terminal outcome:

- completed;
- failed;
- cancelled.

Timeout is represented as a failure with a timeout classification, not as cancellation. Receiving HTTP 404 or 500 is a completed HTTP exchange, not a transport failure.

### 6.3 Cancellation is a state transition, not thread interruption

Cancellation records intent, wakes the owning engine, tears down or detaches the affected transfer, and completes waiters/callbacks. It does not kill arbitrary threads and cannot undo an operation already processed by the remote server.

### 6.4 The reactor owns network objects

Only the engine/reactor manipulates transfer state, sockets, TLS sessions, connection-pool entries, and backend handles. Other threads communicate through commands and immutable or synchronized request state.

### 6.5 Do not call user code while internally borrowed or locked

User callbacks must never execute while an engine lock, curl callback frame, connection state borrow, or FFI-sensitive resource is held. Completion is first converted into an owned event and then dispatched.

### 6.6 Make policy explicit

Redirects, certificate verification, body limits, timeouts, callback execution context, queue limits, and shutdown behaviour must be documented settings rather than accidental backend defaults.

### 6.7 One Engine is one bulk-cancellation domain

Individual requests are cancelled by `RequestHandle` or Engine-scoped `RequestId`. There is no `Client::cancel_all()`. `Engine::cancel_all()` cancels all requests accepted before its defined cancellation barrier while leaving the Engine available for later work. `Engine::shutdown()` stops admission permanently, cancels outstanding requests, drains or suppresses callback delivery according to policy, and tears the Engine down.

If a component needs bulk cancellation that must not affect other work, it creates another Engine or retains and cancels its own request handles. A lightweight request set may be added later if retaining many handles proves awkward; cancellation grouping is not hidden inside Client.

### 6.8 Dispatch only owned events at safe points

The reactor/backend never invokes user code directly. It commits request state, creates an owned event, releases internal borrows/locks, and enqueues that event for dispatch.

The default spawned mode dispatches callbacks away from the network reactor using exactly one Engine-created worker. An explicit `callback_workers(n)` setting enables concurrency between different requests. Events for one request remain serialized and ordered at every worker count. The callback queue is bounded, progress may be coalesced, and terminal events are never silently discarded.

The callback dispatcher is an Engine-created domain, not something attached to any one Client. Callback closures may naturally capture Client clones. If the Engine has stopped, those Clients remain valid values but every Engine-dependent operation fails with `EngineStopped`.

During normal spawned shutdown, the Engine seals the callback queue after all terminal events have been published, waits for queued and running callbacks to return, and joins the dispatcher. A timed shutdown may transfer a still-draining sealed dispatcher into a `DetachedCallbacks` handle. The detached domain owns only callback jobs, closures, completion data, worker join handles, and its workers: it has no network access and does not keep the Engine alive. `is_complete()` becomes true only after every worker thread has exited; `wait()` additionally joins those threads before returning. A callback merely returning is not yet sufficient for DLL unload.

Manual mode can drain the same queue inline on the thread calling `drive()`, after the current readiness/processing pass has reached a safe point. Inline callbacks may submit/cancel or request deferred shutdown; they may not block waiting on the same Engine, recursively drive it, or synchronously join/destroy it.

## 7. Conceptual architecture

```text
Application / DLL wrapper
        |
        +---- owns Engine ---- cancel-all / shutdown / drive (manual)
        |          |
        |          +---- client() -> cheap cloneable Client command handles
        |          |                    |
        |          |                    +---- submit / cancel(request ID) / wait
        |          |
        |          +---- blocking waiter adapter
        |          |
        |          +---- bounded owned event queue
        |                     |
        |                     +---- one callback worker (spawned default)
        |                     +---- explicit N-worker pool (opt-in)
        |                     +---- host drain/custom dispatcher
        |                     `---- inline safe-point drain (manual mode)
        |          |
        |          v
        |     Backend contract
        |
        +---- Curl Multi backend (first delivery)
        |
        `---- Rust-native backend (destination)
                  |
                  +---- DNS service
                  +---- polling reactor
                  +---- nonblocking TCP
                  +---- TLS state machine
                  `---- HTTP/1.1 + connection pool
```

The backend contract is internal. It should express commands and owned events rather than expose curl handles, futures, sockets, or TLS types.

## 8. Proposed public model

Names are illustrative and may change during API design.

```rust
pub struct Engine { /* unique lifecycle and cancellation-domain owner */ }
pub struct EngineConfig { /* backend, limits, TLS, run and dispatch modes */ }
pub struct EngineBuilder { /* backend, limits, TLS, run and dispatch modes */ }
pub struct Client { /* cheap cloneable Engine-issued command handle; no public new */ }
pub struct Request { /* method, URL, headers, body, options */ }
pub struct Response { /* status, headers, body, metadata */ }
pub struct RequestId { /* unique within one Engine */ }
pub struct RequestHandle { /* Engine-bound ID and individual cancellation */ }
pub struct PendingRequest { /* accepted request plus direct terminal waiter */ }
pub struct DetachedCallbacks { /* observable handle to a sealed draining dispatcher */ }

pub enum Completion {
    Completed(Response),
    Failed(Error),
    Cancelled,
}

pub enum ShutdownOutcome {
    Complete,
    CallbacksRemaining(DetachedCallbacks),
}

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Engine, Error>;
    pub fn client(&self) -> Client;
    pub fn cancel_all(&self);
    pub fn drive(&mut self, deadline: Instant) -> Result<DriveStatus, Error>;
    pub fn shutdown(self) -> Result<(), ShutdownError>;
    pub fn shutdown_for(self, duration: Duration) -> Result<ShutdownOutcome, ShutdownError>;
}
```

Callback-oriented use:

```rust
let handle = client.start(request, move |completion| {
    // Runs according to the Engine's documented dispatch policy.
})?;

handle.cancel();
```

Blocking use:

```rust
let response = client.execute(request)?;
```

Collective failure and shutdown:

```rust
engine.cancel_all(); // cancel current work; Engine remains usable
engine.shutdown()?;  // reject new work, cancel, drain, join, stop

match engine.shutdown_for(duration)? {
    ShutdownOutcome::Complete => {}
    ShutdownOutcome::CallbacksRemaining(callbacks) => callbacks.wait()?,
}
```

Callback dispatch always begins with an owned event queue. Spawned Engines default to exactly one Engine-created off-reactor callback worker; `callback_workers(n)` explicitly opts into cross-request parallel dispatch. Manual Engines may drain inline after a safe processing pass. Host-drained and custom dispatch adapters are supported design directions, particularly for GUI and FFI consumers.

## 9. Request lifecycle

An illustrative lifecycle is:

```text
Created
   |
Submitted
   |
Queued -> Resolving -> Connecting -> Handshaking -> Sending -> Receiving
   |          |            |              |            |          |
   +----------+------------+--------------+------------+----------+
                              cancel/fail
                                  |
                   Completed / Failed / Cancelled
```

Requirements:

- A request ID is unique within its Engine and carries or is checked against an Engine identity. Using an ID with a different Engine fails closed; it can never accidentally match a recycled numeric slot. If IDs are reused internally, generation counters prevent stale cancellation from affecting a later request.
- `cancel()` is thread-safe and idempotent.
- Cancelling an already-terminal request is successful idempotent no-op, including after its Engine has stopped. A same-Engine ID cannot resurrect work; a wrong-Engine ID still fails closed.
- Cancellation before backend admission still produces the defined terminal outcome.
- Completion racing cancellation has one winner. The loser observes the existing terminal state and does nothing.
- The backend-independent request registry arbitrates that winner and wakes direct waiters synchronously. A winning cancellation then wakes the reactor so backend resource teardown follows; terminal notification does not falsely claim that a remote peer never observed the operation.
- Cancellation submission wakes the Engine immediately; a long periodic polling interval is never the normal correctness mechanism. An interruptible backend must also have a short bounded safety wait so a failed external wake cannot strand the reactor indefinitely. Wake failure is latched as an Engine failure rather than silently ignored. WP2 must measure and set an explicit supported-platform latency gate before the curl/GDS milestone rather than allowing “prompt” to remain subjective.
- In manual mode, cancelling a submission that has not yet been drained makes the request terminal but does not remove its queued command. Command-queue capacity may therefore continue to report `QueueFull` until the host calls `drive()`; cancellation does not secretly drive a manual Engine.
- `cancel_and_wait()` returns only once the request is terminal and its network resources are no longer owned by active engine work.
- `Engine::cancel_all()` affects requests accepted before its cancellation barrier and leaves the Engine running. Its treatment of simultaneously submitted requests must be deterministic.
- An Engine entering shutdown rejects new submissions through every Client.
- There is no Client-wide bulk cancellation. Components retain their request handles or use a separate Engine when they need an independent cancellation domain.
- Dropping a `RequestHandle` does not implicitly cancel; explicit cancellation avoids surprising fire-and-forget behaviour. A named cancel-on-drop guard is desirable but optional if it is not straightforward in the initial API.

## 10. Blocking API contract

The blocking API is an adapter over an accepted request and a terminal-state waiter. It must not call blocking DNS, connect, TLS, send, or receive operations on the caller's thread.

Blocking completion does not run through the user-callback dispatcher. The Engine commits exactly one canonical `Completion` to request state, wakes any blocking waiter directly, and separately queues an owned callback event only when that request has a callback observer. A blocked or detached callback domain therefore cannot delay a blocking result.

Consequences:

- Many caller threads may wait on one client without creating equivalent network worker threads.
- `RequestHandle::cancel()`, `Client::cancel(request_id)`, `Engine::cancel_all()`, or `Engine::shutdown()` wakes affected blocking callers promptly.
- `PendingRequest::wait()` receives the same canonical `Completion` as a callback request.
- Poisoning or panic in one caller must not poison the engine.
- Waiting may optionally accept an external deadline independent of the request's network timeout.

For callers that need individual external cancellation while another thread blocks, the API may provide a two-step form:

```rust
let pending = client.submit(request)?;
let cancel_handle = pending.handle();
let completion = pending.wait();
```

`client.execute(request)` is sugar for submission plus direct waiting. It maps terminal state as follows:

- `Completion::Completed(response)` becomes `Ok(response)`, including HTTP 4xx and 5xx;
- `Completion::Failed(error)` becomes `Err(ExecuteError::Failed(error))`;
- `Completion::Cancelled` becomes `Err(ExecuteError::Cancelled)`.

Rejection before request acceptance—such as invalid input, queue pressure, or `EngineStopped`—remains a submission error rather than a fabricated `Completion`.

The convenience `execute()` call is sufficient when the request's own deadlines or Engine-wide shutdown are the only required cancellation paths. Callers needing individual cancellation from another thread use `submit()`, retain a `RequestHandle`, and wait on the `PendingRequest`.

A waiter-local `wait_for(duration)` timeout does not cancel or terminally fail the request. It returns the still-usable `PendingRequest` (or an equivalent outcome retaining it), allowing the caller to wait again, cancel, or hand it elsewhere. By contrast, expiry of a timeout configured on the Request produces `Completion::Failed(Timeout)`.

Waiting never drives a manual Engine. It is valid when another thread continues to call `drive()`. Waiting on the sole driving thread is forbidden because it would deadlock. Single-thread manual code submits first and uses `Engine::drive_until(pending)` to progress the Engine and dispatch callbacks only at safe points.

## 11. Callback and event contract

Events may include:

- request accepted;
- upload progress;
- response headers available;
- response body chunk available;
- download progress;
- terminal completion.

Only terminal completion is mandatory for every accepted request. Intermediate events can be disabled and may be coalesced.

Callback requirements:

- no callback before `start()` has successfully returned its handle;
- terminal callback exactly once for every accepted callback request, including cancellation caused by normal or timed shutdown;
- no callback after blocking `shutdown()` returns;
- if timed shutdown returns `CallbacksRemaining`, callbacks may continue only within the returned sealed domain; no new callback event can be added;
- callbacks are first queued as owned events and are isolated from the reactor and internal locks;
- events for one request are invoked in order with at most one callback for that request active at once;
- callbacks for different requests may execute concurrently when a worker pool is configured;
- callback panic is contained within Rust and converted to diagnostics; it never unwinds across FFI;
- event queues are bounded, with progress coalescing rather than unbounded growth;
- completion events are not silently dropped due to queue pressure.

Bounded admission accounts for terminal callback pressure using independent limits. Every accepted request holds one global accepted/inflight permit until terminal commit, except that a request with a callback retains that permit until the callback returns. A callback-bearing request also holds a callback-event permit until return. This ensures terminal callback events always fit without dropping or invoking inline under pressure, while blocking-only traffic is not unnecessarily limited by callback capacity. Long-running callbacks therefore apply documented admission backpressure. Pending progress events may be coalesced or displaced to preserve terminal capacity. The command queue remains a separate bound on submissions not yet drained by the reactor.

Callback activation is tracked across admission. User code is enqueued only after the request registry lock is released, and shutdown waits for any in-progress activation before sealing the callback domain. No fast-completion race may invoke user code while admission or registry state is locked.

Callbacks may submit and cancel requests through captured Clients and may signal the unique Engine owner that shutdown is requested. They do not acquire or consume another Engine owner. A callback must not synchronously wait on or recursively drive its own Engine; actual shutdown is performed by the owner only after the callback/drive frame has unwound.

If a user-imposed shared owner nevertheless consumes Engine shutdown on the Engine's own callback stack, NBReq rejects the synchronous join with `ReentrantOperation`, closes admission, and defers cleanup off that stack so it cannot deadlock by joining itself. This is a misuse recovery path, not the normal shutdown API.

NBReq cannot interrupt arbitrary user callback code safely. A callback already running when shutdown begins may finish unrelated work normally. Any Client captured by it is detached from the stopped Engine and returns `EngineStopped` for submission, cancellation, or other Engine-dependent operations.

## 12. Shutdown contract

Engine and spawned callback-domain lifecycles are related but distinct:

```text
Engine:          Running -> ShuttingDown -> NetworkStopped -> Stopped
Callback domain: Open    -> Sealed       -> Draining       -> Complete
```

Both normal and timed shutdown will:

1. atomically stop new submissions;
2. mark all nonterminal requests for cancellation;
3. wake the backend/reactor;
4. make all blocking waiters terminal;
5. publish the required terminal events and discard/coalesce only permitted nonterminal progress;
6. close pooled and active connections;
7. stop and join the reactor, DNS, and every other network-side service;
8. seal the callback queue so no producer can add another event.

`shutdown(self)` then waits without a library-imposed timeout for all queued and running callbacks to return, joins the dispatcher workers, and reports complete. This is the default and expected behaviour.

`shutdown_for(self, duration)` performs the same irreversible network shutdown, then waits up to the supplied duration for the sealed callback domain. It returns:

- `ShutdownOutcome::Complete` if all callbacks completed and dispatcher workers joined; or
- `ShutdownOutcome::CallbacksRemaining(DetachedCallbacks)` if callbacks remain, transferring observable ownership of the self-draining dispatcher while allowing the stopped Engine object to be destroyed.

A zero duration is the nonblocking test: it returns `Complete` only if dispatch is already finished, otherwise it returns the handle. Timing out does not cancel user code and does not restart or leave the Engine half-running.

`DetachedCallbacks` is an independent, unique, non-cloneable observation/ownership handle and provides at least completion/status inspection plus `wait()` and `wait_for(duration)`. A single obvious handle owns final-wait and DLL-unload responsibility; callers that need several observers coordinate around it explicitly. Dropping this handle does not interrupt callbacks; the sealed domain remains self-owned until its queue drains and its running callbacks return. Once detached, it cannot receive more work and it has no access to the dead Engine's reactor, connections, resolver, or request registry.

Callback detachment applies only after network-side shutdown is complete. An unjoined resolver, reactor, TLS operation, or other Engine service is a shutdown failure, not a detachable callback. The API must never imply that a callback handle makes such network work safe to abandon.

Closing request admission and completing shutdown are distinct facts. Once shutdown begins, admission remains permanently closed even if backend teardown reports an error. The callback queue is still sealed, incomplete backend state is retained for the internal Drop fallback to retry, and a later cleanup attempt cannot report success merely because admission was already closed.

Explicit shutdown consumes the unique Engine owner, so callers cannot continue using a public stopped Engine value. Shutdown initiation remains internally idempotent so races, cleanup paths, and the Engine destructor converge safely on the same transition.

Engine `Drop` performs the normal draining shutdown when explicit shutdown was not used. Consequently, dropping an Engine can wait forever if arbitrary user callback code never returns. This is deliberate memory/DLL safety, not a bounded-shutdown promise. Code that may need to recover a long-lived callback domain must call `shutdown_for`, including zero duration for the immediate-detach form, before losing the Engine value. GDS uses explicit normal `shutdown()` and never relies on Drop or timed detachment on its DLL unload path.

Dropping a Client never shuts the Engine down. Surviving Clients observe `EngineStopped` after the shutdown barrier.

Manual mode has no separate dispatcher lifetime: callbacks run only while the host is driving the Engine. If user code blocks that thread, it is already blocking all manual progress. Manual teardown completes after the active drive/callback frame unwinds and the host drains its remaining terminal events.

## 13. Timeouts

The portable curl/native contract initially exposes three monotonic time controls:

- connect timeout;
- inactivity/idle timeout;
- total deadline, including queue time.

Timeout is a failure, distinct from cancellation. A timeout error identifies the portable category that expired.

The public inactivity timeout means elapsed monotonic time without useful I/O progress across resolution, connection establishment, request send, or response receive; it is not defined as an average transfer-rate threshold. It can therefore expire during slow DNS/connect even when a separate connect timeout is longer. A backend that cannot represent this meaning and duration honestly must reject the option or document a deliberately narrower pilot capability. The curl pilot may use private progress callbacks and bounded reactor passes to implement the clock, but must not expose `CURLOPT_LOW_SPEED_TIME` rounding or low-speed semantics as though they were the portable contract.

The native destination may eventually distinguish:

- queue timeout;
- DNS timeout;
- connect sub-stages already covered by the portable connect timeout;
- TLS handshake timeout;
- request send timeout;
- response header timeout;
- response-body idle timeout;
- total request deadline.

Finer native stages are diagnostic extensions until both backends can represent them honestly. Unsupported distinctions must not be silently approximated differently by each backend. Elapsed-time decisions use monotonic clocks.

## 14. DNS

Portable system name resolution can block. The initial native backend may use a small owned resolver service so the reactor never blocks. Cancelling a request abandons the resolution result immediately even if the operating-system resolver call cannot itself be interrupted.

Shutdown of outstanding system resolver calls is a hard lifecycle concern. Before the native backend is production-ready, the implementation must either:

- prove that its resolver threads have a bounded, joinable shutdown on supported platforms;
- use cancellable platform resolver APIs; or
- provide a nonblocking DNS implementation.

The curl pilot does not redefine this requirement. A controlled Ubuntu 20.04 test of distribution
libcurl 7.68's threaded resolver proves Engine shutdown joins the resolver and leaves no extra
thread, but cancel-to-network-shutdown follows the blocking `getaddrinfo` duration (1.703 seconds
for a deliberate 1.5-second stall). That package therefore has an explicit pilot limitation and
cannot claim the provisional prompt-cancellation gate for DNS. A curl pilot that needs that claim
must select and prove a cancellable resolver such as c-ares; the Rust-native destination still owns
the stronger contract above. The current stepping-stone deliberately accepts this named limitation,
uses finite connect and total deadlines, and does not add curl-only resolver machinery merely to
imitate the eventual native design.

DNS caching, expiry, IPv4/IPv6 ordering, and Happy Eyeballs belong to the native-backend work plan.

## 15. HTTP/1.1 correctness requirements

The native backend must correctly handle at least:

- fragmented status lines and headers;
- configured limits on header bytes and header count;
- informational responses before the final response;
- requests whose method or final status forbids a response body;
- `Content-Length`, including conflicting or invalid values;
- `Transfer-Encoding: chunked`, including chunk extensions and trailers;
- the precedence of transfer encoding over content length;
- close-delimited bodies;
- premature EOF and length mismatch;
- persistent-connection eligibility;
- server-requested connection close;
- redirects with a defined method/body/authentication policy;
- cancellation at every input boundary;
- arbitrary network fragmentation, including one byte per read.

The implementation should use `httparse` for response-head recognition and its chunk-size parser where suitable. Body framing and connection semantics remain NBReq responsibilities.

## 16. TLS

The native backend uses pinned `rustls` 0.23.42 with an explicit Ring provider, driven as a sans-I/O state machine over NBReq's nonblocking reactor without an async runtime. Verified system trust uses pinned `rustls-platform-verifier` 0.7.0; generated fixtures inject a private test root without changing an operating-system store. System DNS configuration is read by pinned target-specific `ipconfig` 0.3.4 on Windows and `resolv-conf` 0.7.6 on Unix; neither owns query execution or introduces an async runtime.

DNS UDP truncation falls back to an NBReq-owned nonblocking TCP connection on the resolver poll owner. The length prefix and response are incrementally bounded; cancellation and Engine shutdown close that connection and join the resolver exactly like the UDP path.

Native DNS caching is Engine-local and bounded; it is never process-global. Positive records respect an upper TTL clamp, zero-TTL records are not cached, and negative results are cached only when an authoritative DNS response supplies a valid negative lifetime. Expired entries are never delivered, and capacity is enforced before insertion.

Requirements:

- certificate and hostname verification enabled by default;
- explicit trust-root configuration;
- platform/native roots or bundled roots as an accepted build choice;
- SNI and ALPN configured deliberately;
- no global insecure mode;
- the existing GDS no-verify configuration remains supported for deployments that still require it; the bypass is explicit, prominently named, never the library default, and recorded in safe diagnostics. The likely legacy motivation is an older installation's trust store or TLS backend not recognising a newer issuing chain/root, but WP4/WP5 must confirm that history rather than encoding the recollection as security policy;
- integration tests must determine whether the legacy switch disables chain validation, hostname validation, or both, and map that behaviour deliberately rather than broadening it accidentally;
- TLS errors are distinguishable from TCP and HTTP errors;
- cancellation works during handshake and encrypted reads/writes;
- TLS handshake and record buffering is bounded before growth independently of HTTP plaintext
  limits;
- a close-delimited HTTPS response completes only after authenticated TLS `close_notify`; raw TCP
  EOF is a receive failure and cannot silently weaken truncation protection.

The curl pilot's generated local fixture already proves verified success with a request-scoped
direct trust anchor, wrong-host rejection, unknown-root rejection, expired-certificate rejection,
and the explicit chain-and-hostname no-verify compatibility path against both the SSL-enabled test
build and the exact pinned Windows Schannel DLL. It does not modify the OS trust store or check in a
private key. Ten deliberately stalled Windows TLS-handshake trials also prove cancellation closes
the peer socket inside the provisional 100 ms gate. Native Ubuntu 20.04 with system libcurl
7.68/OpenSSL 1.1.1f now passes the same policy and interruption matrix; test-only custom trust falls
back to a uniquely owned CA file because libcurl's in-memory CA option begins at 7.77. The exact
Windows package under stock Wine 5 passes explicit no-verify and TLS-handshake interruption, but
Wine 5 Schannel rejects the generated custom trust anchor; that legacy trust limitation remains a
named compatibility constraint rather than a relaxation of verified-by-default policy. Exact GDS
setting parity remains open; the native backend must reuse the same policy cases.

The desired full native packaging is a self-contained executable or DLL. Platform libraries and statically linked cryptographic implementation details are acceptable. The curl stepping-stone is a pilot deployment and may ship a pinned curl DLL plus its audited runtime dependencies beside GDS. Windows 10 already provides `bcryptprimitives.dll!ProcessPrng`; stock Wine 5 does not. If the final Rust-built Windows GDS artifact imports that API, the Ubuntu 20.04/Wine-5 deployment may carry NBReq's audited one-export compatibility shim beside it. That shim delegates to Wine 5's existing `BCryptGenRandom`, is independent of libcurl, and is not needed on supported native Windows.

## 17. Connection pooling

The Engine owns a bounded pool keyed by the connection-relevant origin and TLS/proxy configuration.

Requirements:

- configurable global and per-origin limits;
- idle expiry;
- validation before reuse through ordinary protocol behaviour;
- no reuse after framing ambiguity, premature EOF, cancellation that leaves unread HTTP/1.1 data, or relevant TLS/socket failure;
- fair admission so one origin cannot starve all others;
- shutdown closes all idle and active connections.

The production-facing Engine configuration uses five immutable values. `max_connections` and
`max_connections_per_origin` are non-zero and count connecting, leased, and idle sockets together.
`max_idle_connections` and `max_idle_connections_per_origin` may be zero; zero in either applicable
scope disables idle retention there. `idle_connection_timeout` is measured from the instant a clean
connection is parked, and zero disables idle retention. Defaults preserve the proven native policy:
32 active globally, 8 active per origin, 32 idle globally, 4 idle per origin, and 30 seconds idle.
The smaller applicable global/per-origin bound always wins, so a per-origin value larger than its
global partner is valid but cannot enlarge the global budget. Accepted requests waiting for a
connection remain admitted under their original total/connect/inactivity clocks and oldest-eligible
fairness; capacity pressure does not create a transparent retry or a second request identity.

WP9.5 observability is an immutable, nonblocking snapshot obtained from the unique Engine owner.
It does not add `Arc<Engine>`, `Engine: Sync`, callbacks, background reporters, reset operations, or
Client-wide inspection of unrelated traffic. The first snapshot includes monotonic accepted,
completed, failed, cancelled, connection-opened/reused/closed, and idle-evicted counters; current
inflight, command/callback queue, streaming-byte, active/idle connection, and connection-waiter
gauges; and high-water marks for the bounded gauges. Values are per Engine, saturating, and may be
slightly cross-field inconsistent while work progresses. No URL, origin, method, header, body,
certificate, address, or backend-native error value is retained. Timing/stage histograms require a
later explicit privacy and cost decision rather than appearing accidentally in the first snapshot.
Native opened/closed counters follow the same capacity lifecycle as the active gauge: the lifecycle
begins when DNS/connect capacity is reserved and can therefore close before a TCP handshake. Reuse
counts only a successful lease of an already-clean idle connection.

Initial native milestones may disable reuse until single-request correctness is established.

## 18. Bodies, streaming, and limits

Buffered request and response bodies are required because they cover current GDS JSON, text, and form calls simply.

The buffered pilot starts with conservative configurable Engine limits: 1,024 accepted/inflight requests, 1,024 queued commands, 1,024 callback-bearing requests/events, 16 MiB request bodies, 16 MiB response bodies, 64 KiB of request or response header bytes, and 256 request or response header fields. These are safety defaults rather than permanent product tuning; the GDS audit may justify smaller defaults before release. Limits are checked before admission or extending owned buffers and produce a specific queue/limit failure.

Defaults and hard maxima must remain deliberate for:

- request body size;
- response body size;
- header bytes/count;
- redirect count;
- active requests;
- queued requests;
- active connections;
- queued completion events.

Streaming response support is desirable in the initial public design even if implemented after buffered bodies. It must include backpressure: the network engine may pause reading a transfer rather than buffering without limit.

The native streaming family is separate from the buffered `Request` / `Response` / `Completion`
family:

- `StreamRequest` is a complete request builder. `.body(Vec<u8>)` selects a buffered,
  replayable upload; `.body_stream(UploadBody)` consumes a unique, non-replayable producer body.
  Calling both is rejected by `build()` rather than silently changing modes. `StreamRequest::from`
  `Request` is convenience sugar, not the primary construction path.
- `UploadBody::fixed(length, queue_capacity)` and `UploadBody::chunked(queue_capacity)` return a
  unique `(UploadBody, UploadSender)` pair. Neither end is `Clone`; the caller retains the
  `Send` producer and the Engine receives the body exactly once. Fixed bodies generate and enforce
  `Content-Length`; chunked bodies generate HTTP/1.1 chunk framing. Caller-supplied
  `Content-Length` or `Transfer-Encoding`, request trailers, automatic `Expect: 100-continue`, and
  streaming uploads on GET or HEAD are rejected. `UploadSender::finish(self)` is explicit and
  consuming. Dropping it first fails the send unless a final HTTP response already won.
- `Client::submit_stream(StreamRequest)` returns one unique `ResponseReader`; there is no
  `PendingRequest`, streaming `Completion`, callback path, or second body waiter. Its cloneable
  `RequestHandle` is cancellation-only. Every `StreamRequest` has a streaming response, including
  one with a buffered upload. `ResponseReader::collect(self)` is spawned-mode consumer sugar that
  returns an ordinary `Response`; it fails closed after any response-body byte was consumed and
  never invents a `Completion`.
- The first public head is the final `ResponseHead`; informational responses stay internal. HEAD,
  204, 205, 304, and an explicit zero-length body are already at EOF when that head becomes public.
  More generally, once the final body byte is delivered, dropping the reader does not cancel merely
  because the caller did not perform one extra read. Dropping before EOF cancels and destroys the
  connection. Trailers remain validated framing rather than ordinary response headers.
- Streaming uploads are never replayable. Every redirect response, including 303, is returned
  unfollowed. A buffered upload with a streaming response retains the ordinary buffered redirect
  policy, including replayable 307/308 handling.

`UploadSender::try_push(Vec<u8>)` is nonblocking and all-or-nothing: a full queue or a chunk larger
than that transfer's queue returns the unchanged `Vec`. Blocking `push` is spawned-mode only and may
feed a larger buffer progressively; interruption returns its unsent suffix because accepted bytes
cannot be reclaimed. It wakes for capacity, early final response, cancellation, failure, and Engine
stop. Calling it before successful submission reports `NotSubmitted`; a manual Engine reports
`WrongMode` without driving. Early 4xx/5xx remains a completed HTTP exchange through
`ResponseReader`; the sender merely closes and queued upload chunks are discarded.

Manual-mode producer and consumer methods never block and never drive the Engine. They expose
`try_push`, `try_head`, and `try_read`; progress comes only from the owner's `drive` calls. Blocking
`push`, `wait_head`, `read`, and `collect` fail with `WrongMode` in manual mode. The initial handles
are unique and `Send`, not `Clone` or `Sync`; multi-producer and callback adapters may be built later
without changing the reactor ownership model.

Streaming has two independent resource controls. Each transfer has a small bounded flow-control
window; a full response window pauses that connection before another socket or `read_tls` operation
without stalling the Engine. TLS may consume at most one documented record beyond the nominal
window. The initial default is a 256 KiB maximum window for each upload or response direction. An
Engine-wide queued-byte budget, initially 16 MiB, conservatively reserves the accepted upload and
response windows so their aggregate can never grow beyond it. Pre-submission upload bytes are
caller-owned; acceptance binds the channel to this Engine budget and may reject it. Both values are
Engine construction settings; zero disables streaming admission. Separately, the existing request
and response body limits remain the Engine-owned total-byte ceilings, defaulting to 16 MiB. A
`StreamRequest` may select a smaller clamp but a cloneable Client cannot raise the Engine ceiling;
large or long-lived streams require explicit Engine configuration.

Application backpressure does not count as response inactivity because the network is deliberately
waiting for the consumer. Total time still runs from acceptance. An empty unfinished upload queue
does count as inactivity because its producer has stalled. No user producer, consumer, reader,
writer, or callback code executes on the reactor.

File upload/download convenience can be built over streaming and is not required for the first GDS replacement.

## 19. Error model

The stable top-level error classification should cover:

- `EngineStopped` or Engine shutting down;
- queue/concurrency limit;
- invalid request or URL;
- DNS;
- connect;
- TLS;
- timeout with stage;
- send;
- receive;
- malformed HTTP;
- redirect policy;
- body/header limit;
- callback/consumer failure;
- backend/internal failure.

Cancellation is preferably a distinct terminal result rather than an error classification.

Errors may retain backend-specific diagnostic sources for logs and debugging, but callers must not need to match curl error numbers. Error display must redact credentials and sensitive headers.

The initial stable shape keeps a broad `ErrorKind` while attaching backend-neutral detail where it is meaningful: timeout category, transport stage, and violated resource limit. Curl codes remain diagnostic inputs only. WP3 freezes and proves the consumer API and representative public mappings needed for the curl pilot: HTTP status versus failure, cancellation, TLS, configured timeout, oversize data, unsupported backend capability, and submission pressure. WP4 owns the adversarial send/receive/malformed-response laboratory; native DNS/connect work owns the deterministic stage mappings that the curl pilot explicitly does not claim. The curl API slice may be used for rollback-protected GDS pilots before that complete transport-stage corpus is finished, but the crate is not described as transport-complete or public-release-ready until those later gates pass.

The first WP4 curl corpus drives only public NBReq types. It classifies an abortive connection failure
as Send when curl's observed uploaded byte count shows that the buffered body was incomplete; short
fixed-length and empty responses are Receive; invalid status/header/length/chunk syntax and
incomplete chunk framing are Http. This is an observable portable policy, not a promise to preserve
curl result codes. Valid chunk extensions and trailers may complete, but trailer representation is
still unspecified and callers may not rely on trailers appearing in the ordinary response-header
collection.

## 20. Backend contract and migration

The curl backend is a stepping stone and reference implementation:

- it uses libcurl Multi rather than one blocking easy handle per thread;
- it may be dynamically packaged beside GDS for pilot deployments; static curl is not a milestone requirement;
- it implements only the accepted NBReq public semantics;
- curl handles and error codes remain private;
- cancellation removes the transfer on the owning engine thread;
- curl callbacks produce owned internal events rather than directly invoking user callbacks.

Isahc was considered because it already presents a higher-level Rust HTTP API over libcurl without requiring Tokio. It is not the selected foundation because NBReq still needs its own unique Engine ownership, manual driving, direct blocking waiters, callback-domain detachment, pinned curl policy, and DLL shutdown proof. Wrapping Isahc would retain its lifecycle and policy layer while NBReq rebuilt the machinery that differentiates this project.

The curl backend uses an explicit compatibility profile rather than inheriting whichever behaviours its build happens to provide:

- force HTTP/1.1 while the native portable contract is HTTP/1.1-only;
- disable environment-derived proxies unless proxy use is explicitly configured;
- suppress automatic `Expect: 100-continue` until that exchange is deliberately supported by both backends;
- disable cookie storage and automatic cookie handling;
- disable automatic response decompression and do not advertise compression unless it becomes an accepted portable feature;
- implement the accepted redirect method/body/authentication table explicitly;
- return HTTP 4xx/5xx as completed HTTP responses rather than transport failures;
- accept backend-neutral byte-valued request headers, while documenting the current curl Rust
  binding's UTF-8-only submission constraint; a valid non-UTF-8 value fails as `Unsupported` rather
  than being reclassified as an invalid portable request;
- select and record the TLS backend/trust-root behaviour used by supported Windows/Wine and Linux builds;
- inspect/report relevant runtime curl capabilities and resolver behaviour;
- prove bounded wakeup, cancellation, resolver cleanup, and Engine shutdown for the exact packaged curl build.

The curl backend may retain private pilot constraints where libcurl cannot exactly model the native destination. Such a constraint must be explicit, tested where practical, and absent from backend-neutral public types. It may not silently redefine cancellation, timeout, limits, errors, ownership, or shutdown. Removing the curl backend later must require only backend construction/packaging changes, not a consumer lifecycle rewrite.

The working conservative redirect table is:

| Status | GET | HEAD | POST | Other methods |
|---|---|---|---|---|
| 301 / 302 | follow as GET | follow as HEAD | return the 3xx unless an explicit browser-compatible rewrite policy is enabled | return the 3xx unless an explicit method policy is enabled |
| 303 | follow as GET | follow as HEAD | follow as GET and drop the body | follow as GET and drop the body |
| 307 / 308 | preserve GET | preserve HEAD | preserve method and body if replayable | preserve method and body if replayable |

All automatic redirects have a small configured hop limit. HTTPS-to-HTTP downgrade is blocked by default. `Authorization` and other origin-bound credentials are stripped whenever scheme, host, or effective port changes. A redirect requiring replay of a non-replayable body fails explicitly rather than sending a partial or altered request. This is the accepted portable default; curl/native implementations may not choose independently. Any future browser-compatible or application-specific behaviour must be an explicit opt-in policy.

Curl-global initialization is process/module state, an explicit exception to per-Engine isolation. The upstream Rust `curl` crate normally initializes from a platform constructor, which is loader-sensitive in a Windows DLL. The pinned pilot binding disables that constructor and adds a fallible, once-recorded `curl::try_init()` used when the spawned reactor constructs the backend outside `DllMain`; initialization failure becomes an Engine error rather than poisoning a `Once` and repeatedly panicking. Global cleanup is deliberately not scheduled because the binding cannot prove the surrounding process thread state. Engines therefore never call `curl_global_cleanup()` independently or maintain a misleading per-Engine cleanup reference count. The exact pinned crate, local patch, and packaged libcurl behaviour must be recorded.

Detached callback domains contain no curl handles, resolver work, TLS state, or backend-owned values and therefore do not extend curl backend lifetime. Before a curl Engine reports its network side stopped, every easy/multi handle and any resolver activity from that Engine must be gone.

Windows DLL use is a specific WP2 proof obligation. libcurl warns that global cleanup does not wait for resolver threads and cautions against unloading a module that still contains such activity; it also cautions against initialization from `DllMain` or a DLL static initializer. The pilot decision is conservative: initialize explicitly on the reactor thread, stop and join every Engine-owned handle/resolver activity, preload the pinned curl DLL from a controlled absolute path, and pin both curl and the curl-backed GDS module until process exit. `FreeLibrary`-based unload of the curl-backed GDS DLL is unsupported. Fresh-process load/use/exit repetition is required; in-process unload/reload is not claimed. The native backend is not subject to this pilot restriction.

Lifecycle references: <https://docs.rs/curl/latest/curl/fn.init.html>, <https://curl.se/libcurl/c/libcurl.html>, and <https://curl.se/libcurl/c/curl_global_cleanup.html>.

Curl may internally use IPv6 racing or Happy Eyeballs even before the native backend implements the same connection strategy. This is an allowed backend implementation difference, not a portable scheduling guarantee, provided address-family correctness, cancellation, timeout classification, and externally observable request semantics pass the shared contract.

The Rust-native backend will be accepted when it passes the same black-box contract suite. Both backends may coexist behind Cargo features during development. Feature-implicit curl selection is accepted only for the private pilot. Before public crate release, Cargo features determine which backends are available while Engine configuration explicitly selects the backend (or an unambiguous documented default); dependency feature unification must not silently change which transport `Engine::new(config)` constructs.

Mutating requests must never be sent to both backends merely to compare results. Backend differential tests use controlled test servers, recorded fixtures, or idempotent synthetic requests.

## 21. Engine run and dispatch modes

Two execution models are first-class. They share the same request, cancellation, event, and terminal-result semantics.

### 21.1 Spawned Engine

- the public Engine owner is `Send`; moving it does not move the reactor, which remains on its owned thread;
- the Engine owns one network reactor thread;
- calling `drive()` on a spawned Engine returns an explicit `WrongMode`/`AlreadyDriven` error and never attempts to drive the owned reactor;
- submission and cancellation through Client are thread-safe;
- blocking callers wait independently and never execute network I/O;
- the reactor queues owned callback events;
- callbacks default to exactly one Engine-created dispatcher worker, never the network reactor;
- `callback_workers(n)` explicitly enables a larger pool and cross-request callback concurrency;
- per-request callback ordering and serialization are preserved across the pool;
- normal shutdown stops and joins the reactor, seals and drains callback dispatch, and joins dispatcher workers;
- timed shutdown may return the observable sealed dispatcher after all reactor/network services have joined, allowing callbacks to finish without keeping the Engine alive.

### 21.2 Manual Engine

- the native manual Engine is designed to be `Send`, permitting sequential ownership transfer between threads but never concurrent drive;
- the host owns the Engine and calls `poll`/`drive` with a deadline;
- only manual mode accepts `drive()`; nested or concurrent drive fails rather than re-entering the reactor;
- no network or callback worker is created unless explicitly configured;
- after a processing pass reaches a safe point, queued callbacks may run inline on the driving thread;
- inline callbacks may enqueue submit/cancel/deferred-shutdown commands, which are applied only at a safe turn;
- blocking `execute()` on the driving/callback stack and nested `drive()` are initially forbidden; an explicit outer `drive_until()` may provide single-thread blocking convenience;
- teardown/destruction is deferred until the current dispatch pass unwinds.

Host-drained queues and custom dispatchers are possible in either model where their lifecycle contract is explicit. Manual mode need not be implemented before the curl-backed GDS milestone, but the initial type/API boundary must not assume that every Engine always owns a thread.

The current Rust `curl::multi::Multi` binding is `!Send` and `!Sync`, although libcurl's C contract permits sequential handle transfer between threads and forbids only simultaneous use. Therefore spawned curl can satisfy `Engine: Send` by keeping `Multi` inside its reactor thread, while manual curl is initially unsupported or explicitly thread-bound. NBReq will not add an unsafe `Send` wrapper merely for backend symmetry; any later wrapper requires an audit of the pinned binding, callbacks, TLS, destruction, and sequential transfer. The native manual destination must not inherit this curl limitation.

Threading references: <https://docs.rs/curl/latest/curl/multi/struct.Multi.html> and <https://curl.se/libcurl/c/threadsafe.html>.

## 22. FFI and DLL requirements

An optional FFI layer will use opaque handles and an explicit calling convention. It must:

- validate null pointers, lengths, UTF-8/byte ownership, and handle state;
- catch Rust panics at every exported boundary;
- never unwind through foreign frames;
- specify who owns request/response buffers and callbacks;
- provide explicit Engine shutdown/free;
- make normal shutdown/free wait until callbacks finish;
- if timed detachment is exposed, return an opaque waitable callback-domain handle and state clearly that Engine-free is not DLL-unload permission while that handle remains incomplete;
- retain foreign callback context and the loaded module until detached callbacks complete, or prohibit detachment in adapters that cannot prove those lifetimes;
- avoid doing significant work from `DllMain` or loader callbacks;
- make cancellation and status polling available without requiring the host to understand Rust futures.

The core crate remains safe Rust wherever practical. Necessary OS and FFI unsafe code is isolated and documented.

## 23. Security and observability

- TLS verification is on by default.
- URL credentials, `Authorization`, cookies, request bodies, and response bodies are not logged by default.
- Debug wire logging requires explicit opt-in and redaction hooks.
- Header/body limits are enforced before unbounded allocation.
- Redirects do not forward credentials across origins unless explicitly permitted.
- Metrics identify request IDs, stages, byte counts, durations, cancellation, timeout, pool reuse, and queue pressure without payloads.
- Faults in one request do not corrupt other request state.

## 24. Compatibility targets

Initial supported environments proposed for discussion:

- Windows 10 x64 or later, including use from the GDS Rust `cdylib`;
- the Windows build under the distro-default Wine supplied by Ubuntu 20.04;
- native Linux x64 on Ubuntu 20.04;
- stable Rust with a documented minimum supported Rust version;
- a curl pilot with pinned adjacent runtime libraries and an audited dependency inventory;
- a self-contained native release to the extent allowed by platform system libraries.

These are initial targets rather than permanent exclusions. If toolchain, TLS, or curl constraints make a target impractical, the exact version may be varied deliberately and recorded before the relevant milestone rather than silently weakened.

## 25. Acceptance criteria for the first useful delivery

The curl-backed pilot is useful when:

- an Engine performs concurrent GET and POST requests through cloneable Clients;
- blocking and callback forms share behaviour;
- GDS WebRPC long polling can be cancelled promptly;
- dropping/stopping the GDS owner does not leave detached HTTP workers;
- normal GDS shutdown drains and joins its short-lived callbacks rather than using timed detachment;
- every accepted request reaches exactly one terminal state;
- total, connect, and idle/response timeouts behave monotonically;
- queue and body limits are enforced;
- the DLL can shut its Engine down and unload safely;
- any detached callback handle prevents DLL unload from being reported safe until it completes;
- focused cancellation, race, malformed-response, and shutdown fault tests pass on Windows and Linux;
- the exact curl DLL and every transitive runtime dependency are pinned, packaged beside the pilot, and loaded from a controlled location;
- startup verifies or records the loaded curl version/capabilities and does not silently accept an unrelated ambient DLL from `PATH` or another search directory;
- GDS can select the NBReq/curl binding or retain the existing ureq implementation through configuration for immediate rollback.

The Rust-native public release is useful when it meets the same contract for the accepted HTTP/1.1 scope, passes backend parity tests, and GDS can switch backend without application-level changes. Full publication/rollout may wait for this milestone rather than promoting the curl pilot as the finished product.

## 26. Accepted and deferred review items

Accepted answers form the WP0 contract. Unresolved items below are policy, integration-audit, or proof work and do not block WP0.

1. **Name and home:** `NBReq` remains the working name; the final public name remains open. The standalone repository/crate boundary beside GDS was accepted and established in WP0.

2. **Cancellation notification — accepted:** An explicitly cancelled accepted request receives exactly one `Cancelled` terminal callback. Silent disappearance would make ownership and cleanup harder.

3. **Handle drop — accepted:** Dropping the last `RequestHandle` allows the request to continue; cancellation is explicit. A named cancel-on-drop guard is desirable but optional if it is not straightforward in the initial API.

4. **Shutdown callback policy — accepted in principle:** Normal shutdown publishes the required terminal events, seals the queue, and waits for callbacks to finish. Timed shutdown may return the sealed callback domain; it does not silently suppress accepted terminal delivery.

5. **Long-lived callbacks — accepted in principle:** Arbitrary callback code is never interrupted. Normal shutdown waits. Timed shutdown stops all network work, seals the callback domain, and returns `Complete` or a waitable `DetachedCallbacks` handle. The Engine may then die; captured Clients return `EngineStopped`. DLL unload remains unsafe until the detached domain completes.

6. **Callback pool shape — accepted in principle:** Spawned mode defaults to exactly one off-reactor callback worker. `callback_workers(n)` explicitly opts into cross-request concurrency. Per-request serialization/order remains mandatory for every size.

7. **Initial body model — accepted:** Buffered request/response bodies are sufficient for the curl/GDS pilot. Reserve the API shape for streaming and enforce limits from day one; streaming follows with the native/full scope.

8. **Redirect defaults — accepted:** Use the conservative table in Section 20: 301/302 do not rewrite POST unless browser-compatible behaviour is explicitly enabled; 303 becomes GET except HEAD; 307/308 preserve method/body only when replayable; block HTTPS downgrade by default; strip origin-bound credentials across origins.

9. **TLS trust roots:** Prefer OS roots, bundled web roots, or a configurable choice?\
   Recommendation: configurable, with OS/native roots for GDS if reliable under Wine and bundled roots as a deterministic alternative.

10. **Insecure TLS — accepted compatibility requirement:** Preserve the current GDS no-verify configuration. Verification remains the NBReq default; bypass is unmistakably explicit and reported in safe diagnostics. Confirm and test the legacy hostname/chain semantics during integration.

11. **Native DNS milestone:** Is an owned blocking resolver service acceptable initially, provided request cancellation is prompt, or must Engine shutdown also cancel the underlying resolver immediately?\
    Recommendation: accept the worker for the first native slice, but do not declare DLL-safe production readiness until bounded resolver shutdown is proven.

12. **Platform gates — accepted initial targets:** Windows 10 x64 or later; the Windows build under Ubuntu 20.04's distro-default Wine; and native Linux x64 on Ubuntu 20.04. Versions may be varied if a concrete toolchain/backend problem is demonstrated and the change is recorded.

13. **HTTP scope:** Does any known GDS path require proxies, response compression, multipart file upload, cookies, client certificates, or methods beyond GET/POST in the first release?

14. **Manual blocking convenience — accepted in principle:** `wait()` never drives an Engine. Another thread may wait while the host drives; single-thread manual code uses `Engine::drive_until(pending)`. Waiting or nested driving on the current drive/callback stack is forbidden.

15. **Curl packaging — accepted for pilot:** Static linking is not required. Ship a pinned curl DLL and its audited dependencies beside GDS, using a controlled load location. Curl deployments are pilots with ureq configuration rollback retained; broad/full release may wait for native.

16. **Licensing and publication — direction accepted:** Aim for a public crates.io library. Choose MIT, Apache-2.0, or the customary dual `MIT OR Apache-2.0` grant after confirming GDS compatibility and dependency notices; the exact choice remains open.

17. **Cancellation latency gate — provisional Windows value recorded:** The exact dynamic Windows package must release controlled slow-header and stalled-body sockets in less than 100 ms after cancellation; current 10-trial maxima are below 4 ms. The same 100 ms target is provisional for connect and for Windows 10, Wine, and Linux until named-stage measurements run there. Never leave “prompt” as the only acceptance language or silently weaken the gate when another platform is measured.

18. **Engine thread traits — accepted in principle:** Ordinary Engine targets `Send` but does not initially promise `Sync`; Client and RequestHandle target `Send + Sync`; PendingRequest and DetachedCallbacks target at least `Send`. Spawned curl satisfies this without moving `Multi`; manual curl may remain unsupported/thread-bound until audited. User-created shared wrappers are allowed but outside NBReq's ownership contract.

19. **Native streaming ownership — accepted:** Keep buffered `Request` / `Response` / `Completion`
    untouched. `StreamRequest` uses a buffered replayable `.body` or consumes one unique
    `.body_stream(UploadBody)` and always returns one unique `ResponseReader`. The caller creates the
    fixed-length or chunked upload pair before submission. The reader is the only streaming terminal
    path; `collect()` is bounded consumer-side sugar, not another waiter. Manual mode never blocks or
    drives implicitly, streaming uploads never redirect, and per-transfer windows plus an Engine-wide
    queued-byte budget enforce backpressure beneath the Engine's total body ceilings.

## 27. Deferred policy and proof decisions

The architecture is closed for WP0. The following may be settled during normal implementation/review without reopening Engine/Client ownership:

- working name and initial repository location;
- final names and status detail for `shutdown_for`/`DetachedCallbacks`;
- any HTTP scope beyond the accepted buffered curl/GDS pilot;
- TLS trust-root policy beyond the accepted explicit GDS no-verify compatibility switch;
- numeric cancellation latency gates from WP2;
- exact MIT/Apache licensing grant and support promise.

Decisions already accepted in principle:

- Engine is the unique lifecycle, resource, and bulk-cancellation owner;
- Engine is non-cloneable and not wrapped in `Arc` as part of the ownership model; `drive(&mut self)` is exclusive and explicit shutdown consumes the Engine;
- ordinary Engine targets `Send` without initially promising `Sync`; user-created `Arc<Mutex<Option<Engine>>>` ownership is permitted but user-managed;
- Client and RequestHandle target `Send + Sync`; PendingRequest and DetachedCallbacks target at least `Send`;
- Client is a cheap cloneable command handle and has no `cancel_all()` or shutdown ownership;
- every Client is issued by `Engine::client()`; Client has no public constructor and never hides an Engine;
- every Engine is independently constructed from configuration; there is no parent/child Engine relationship;
- multiple Engines are permitted and operationally independent;
- individual cancellation uses RequestHandle/RequestId; Engine cancellation covers everything;
- accepted cancellation produces exactly one `Cancelled` terminal result;
- terminal arbitration and direct waiter wakeup occur in the backend-independent request registry; backend cancellation cleanup follows on the awakened reactor;
- dropping RequestHandle allows continuation; a named cancel-on-drop guard is optional/desirable;
- cancellation after terminal/Engine stop is an idempotent same-Engine success; wrong-Engine IDs fail closed;
- callbacks are always queued as owned events and dispatched only after internal state is safe;
- accepted callback work retains bounded admission until the callback returns; progress may coalesce, but terminal delivery is never dropped;
- callback activation completes outside the registry lock and shutdown waits for activation before sealing;
- spawned mode defaults to one off-reactor callback worker; a larger pool is explicit, with per-request order preserved;
- manual mode may dispatch inline after a safe drive pass and permits no recursive blocking/drive/join;
- blocking requests wait directly on canonical terminal state and never depend on callback dispatch;
- `PendingRequest::wait()` returns `Completion`; `execute()` maps completed to `Ok`, failed/cancelled to distinct errors, and preserves HTTP 4xx/5xx as responses;
- waiter-local timeout does not cancel the request; single-thread manual blocking uses `drive_until`;
- buffered request/response bodies are sufficient for the curl/GDS pilot, with streaming reserved for later scope;
- spawned `drive()` fails explicitly; only manual Engine mode can be driven, and never recursively/concurrently;
- the conservative redirect table in Section 20 is accepted;
- normal spawned shutdown waits for sealed callback dispatch to drain;
- timed spawned shutdown may return an observable, waitable, self-draining callback domain only after every network-side service has stopped;
- `DetachedCallbacks` is a unique, non-cloneable observation/ownership handle; the sealed callback domain remains self-owned if that handle is dropped;
- the callback domain is not attached to a Client; captured Clients survive only as stopped command handles;
- detached callbacks prohibit claiming DLL-unload safety until their handle reports complete;
- the current curl backend disables loader-constructor initialization, initializes once explicitly on the spawned reactor, performs no per-Engine global cleanup, and leaves detached callback domains curl-free;
- the curl-backed Windows DLL pilot is pinned until process exit and does not support `FreeLibrary` unload; the native destination retains the stronger unload goal;
- curl is a dynamically packaged pilot backend, with pinned adjacent dependencies and ureq configuration rollback retained;
- initial targets are Windows 10 x64, the Windows build under Ubuntu 20.04's default Wine, and native Ubuntu 20.04 x64;
- verified TLS remains default while the current explicit GDS no-verify behaviour is preserved and tested;
- the project aims for a public crates.io release; exact MIT/Apache licensing remains to be chosen;
- curl runs an explicit HTTP/1.1 compatibility profile rather than inheriting backend defaults;
- the portable initial time model is connect timeout, inactivity timeout, and total deadline.
