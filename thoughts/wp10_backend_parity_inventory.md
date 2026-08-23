# WP10 backend parity inventory

Status: initial source audit, 2026-08-23. This document starts WP10. It classifies observable
behavior before ordinary backend selection changes; it does not authorize native selection from
`Engine::new`, alter the accepted GDS package, or remove curl/ureq rollback.

## 1. Meaning of parity

Parity means that a consumer can determine before admission whether an operation and its
configuration are supported, and that every supported operation obeys NBReq's portable ownership,
cancellation, terminal, limit, error, and shutdown rules. It does **not** require curl and native
to use the same resolver, TLS stack, pool, socket owner, or thread model.

Differences have four dispositions:

- **Required parity:** portable behavior that must agree before native becomes ordinary/default.
- **Explicit backend limitation:** a deliberate difference reported before admission or
  construction, never silently ignored.
- **Environment gate:** proof belonging to a named host/runtime rather than crate logic.
- **Post-v1:** useful work outside the native HTTP default-switch gate.

## 2. Identity and publication boundary

- The project and proposed public crate name are **NBReq** / `nbreq`, meaning Non-Blocking
  Request. The name intentionally permits later DNS and TCP request interfaces as well as HTTP.
- The project owner confirms authorship and ownership of GDS and authority to license the work
  that informed NBReq. There is no outstanding GDS provenance blocker.
- The intended public license is `MIT OR Apache-2.0`. The manifest field and standard
  `LICENSE-MIT` / `LICENSE-APACHE` files wait only for selection of the displayed copyright
  holder.
- Check crates.io again immediately before a genuine alpha publication; do not publish an empty
  placeholder merely to reserve the name.

## 3. Source-grounded backend inventory

| Area | Native | Curl pilot | WP10 disposition |
|---|---|---|---|
| Ordinary `Engine::new` | Never selects native; private proving constructors only | Selected merely by compiling `curl-pilot`; otherwise the scaffold is selected | **Required redesign.** Backend choice must be explicit or unambiguously native by default; Cargo feature unification must not silently change it. |
| Spawned mode | Supported | Supported | **Required parity** for acceptance, wakeup, cancellation, terminal arbitration, panic containment, callbacks, and consuming shutdown. |
| Manual mode | Supported through the same native state machines | Construction returns `WrongMode` | **Explicit curl limitation.** Do not add an unsafe binding wrapper merely for symmetry. |
| Buffered HTTP | Supported | Supported | **Required black-box parity** for request wire policy, responses, redirects, limits, timeout/error kinds, cancellation, and shutdown. |
| Streaming response/upload | Supported with bounded queues and fixed/chunked producer framing | Capability check returns `Unsupported` before acceptance | **Explicit curl limitation.** Required for native readiness, not for the disposable reference backend. |
| Callback `start` family | Uses the backend-neutral registry and dispatcher | Same | **Required lifecycle parity.** User callbacks remain queued off-reactor; streaming callbacks remain absent on both. |
| DNS implementation | Engine-owned Hickory wire service with joined resolver thread | libcurl/platform resolver | **Internal difference.** Portable HTTP results and bounded shutdown matter; a raw DNS facade is post-WP11. |
| TCP implementation | NBReq mio owner with generation-checked slots | libcurl Multi/easy owner | **Internal difference.** Portable cancellation/close behavior matters; a raw TCP facade is post-WP11. |
| TLS | rustls with platform verification; explicit no-verify keeps handshake signatures | pinned libcurl platform TLS; same explicit compatibility policy | **Required policy parity; environment-specific mechanics.** Generated and platform-store fixtures remain named gates. |
| HTTP connection reuse | Bounded NBReq owner pool with no transparent replay | libcurl connection cache | **Required observable safety**, not identical algorithms. Contamination, framing, cancellation, and redirect rules must agree. |
| Active/idle pool settings | All five public settings are enforced | Curl factory receives only response-body/header limits; pool settings are silently ignored | **Required fix.** Map them honestly or reject non-default/native-only configuration during curl construction. |
| Request lifecycle metrics | Backend-neutral accepted/completed/failed/cancelled and queue gauges | Same registry counters | **Required parity** and shared tests. |
| Connection/pool metrics | Native alone records opened/reused/closed/evicted and active/idle/waiter gauges | All remain zero because curl never attaches connection metrics | **Required contract decision.** Zero must mean documented unavailable/not-owned, or expose capability/availability; it must not look like measured zero activity. |
| Body/header/event limits | Request limits enforced before admission; native enforces response and stream bounds | Request limits are shared; curl receives response body/header bounds | **Required black-box parity**, including `LimitKind` and permit release. Streaming bounds are irrelevant after curl's pre-admission `Unsupported`. |
| Connection limits | Native enforces total/per-origin active and idle bounds | Ignored by curl | Same required construction/capability decision as pool settings. |
| Connect/inactivity/total time | Owner clocks cover DNS/TCP/TLS/HTTP; total begins at acceptance | Total includes acceptance/redirect time; monotonic inactivity collector; connect maps through libcurl | **Required outcome audit.** `TimeoutKind::Unknown` remains an honest curl fallback; prompt curl DNS/connect cancellation is not claimed. |
| Redirects | Shared conservative `redirected_request` policy | Same shared policy, with libcurl auto-follow disabled | **Required parity:** method/body replay, hop limit, credential stripping, and HTTPS downgrade refusal. |
| Request headers | Binary values accepted by the native serializer after portable validation | Non-UTF-8 values return `Unsupported` at submission | **Explicit curl limitation**, already documented; must remain deterministic before network work. |
| HTTP wire policy | HTTP/1.1, no pipelining, generated framing | HTTP/1.1 forced; proxy and auto-follow disabled; `Expect` and invented `Content-Type` suppressed | **Required common v1 policy.** Add shared tests where only one backend is presently exercised. |
| Compression/cookies/proxy/client certs | Outside accepted v1 | Disabled/direct/not exposed | **Deliberate common v1 limitations.** Construction and consumer docs must say so. |
| Global/process lifetime | Engine-owned work joins; no curl global state | Explicit global init; no per-Engine cleanup; curl-backed module pinned to process exit | **Explicit pilot packaging limitation.** It must not become part of native's portable contract. |
| Proof environments | Windows and Ubuntu native; Wine-native not claimed | Windows 10, Ubuntu system curl, and stock Wine 5 pilot evidence | **Environment gates.** GDS canaries name exact artifacts and runtimes. |

## 4. Existing shared evidence and first gaps

`tests/http_adversarial.rs` already runs the same public buffered path against either curl or native
for fragmented fixed/chunked responses, malformed status/headers/framing, premature EOF, and an
abortive large upload. Curl alone currently has the public sequential-reuse case. The public
contract suite checks ownership/thread traits, empty metrics, spawned-drive rejection, and the
intentional manual-mode difference.

The first source audit therefore identifies these concrete gaps:

1. There is no explicit public backend-selection type; feature presence selects curl and can
   override a compiled native implementation.
2. Curl silently ignores five public connection-pool settings.
3. Curl reports native connection/pool metrics as plausible zero values with no availability
   signal or documented backend meaning.
4. The shared black-box suite is far narrower than the combined backend-specific redirect,
   timeout, TLS, limit, cancellation, and lifecycle suites.
5. Native is not yet built through an ordinary consumer constructor, so constructor parity itself
   has not been exercised.

These are WP10 seams. Manual curl, curl streaming, resolver internals, the UTF-8 curl-header
restriction, and curl's process-lifetime pin are deliberate limitations rather than missing native
features.

## 5. Schannel classification is already closed

The plan's 2026-08-17 evidence records that the identical vendored-Schannel test binary was fully
green under the ordinary Windows account and failed only under the restricted Codex token. The
later WP9.4 checkpoint sentence saying three fixtures still failed "on this host" was stale and
made that environment exclusion look open.

WP10 does not need to repair or repeat those fixtures merely for parity. Keep normal-user execution
in the Windows evidence procedure. Reopen only if the exact current binary fails in that already
accepted context; legacy Wine 5 rejecting the generated custom root remains a separate, correctly
named platform limitation.

## 6. P10-01 recommended construction/capability freeze

This is the proposed public shape for review before implementation:

- Add a backend-neutral public `HttpBackend` enum whose `Native` and `Curl` variants exist in the
  API under every feature combination. Cargo features control compiled availability only; asking
  for an unavailable implementation fails at Engine construction with `Unsupported`.
- Add `EngineBuilder::http_backend(HttpBackend)`. Keep backend choice out of `EngineConfig`, which
  remains the immutable resource/policy configuration carried into whichever implementation is
  selected.
- At the authorized default switch, make `Engine::new(config)` and an otherwise unqualified
  `Engine::builder().build()` unambiguous native shorthand. Curl remains an explicit diagnostic or
  reference choice. Until that gate, land and test explicit selection without changing the current
  ordinary constructor.
- Remove the no-feature lifecycle scaffold from ordinary product construction at the switch. It
  remains an internal/test backend, never a third public runtime choice.
- Keep `HttpBackend` independent of the future DNS/TCP facade policy. An Engine using curl for
  HTTP may still gain separately specified native DNS/TCP services after WP11; calling this enum
  `EngineBackend` would prematurely imply otherwise.

Curl can conservatively honor the five existing pool maxima rather than reject or ignore them:

- map total and per-origin active maxima to curl Multi's total/host connection limits;
- cap curl's total idle cache at `min(max_idle_connections,
  max_idle_connections_per_origin)`, which may retain fewer sockets but cannot violate either
  advertised maximum;
- disable reuse when either idle maximum or the idle timeout is zero;
- apply positive idle timeout through curl's maximum connection-age option, rounding downward;
  a positive subsecond value therefore disables reuse rather than exceeding the caller's bound.

This is intentionally an upper-bound contract, not a promise that each backend has identical pool
utilization. Add shared configured-bound tests before accepting the mapping.

Connection/pool metrics remain native-owned. Add an explicit availability flag to the metrics
snapshot (with native `true`, curl/scaffold `false`) and document the affected gauges/counters;
backend-neutral request/lifecycle metrics remain available everywhere. Do not synthesize physical
connection reuse from curl easy-handle activity.

## 7. Initial WP10 sequence

### P10-01 — freeze construction and capability semantics

Decide the public backend-selection shape and the behavior of native-only configuration/metrics
when the reference curl backend is selected. Make silence impossible before changing defaults.

### P10-02 — expand controlled black-box parity

Promote backend-specific buffered fixtures into a shared harness where both backends promise the
same result. Use only local controlled endpoints and idempotent differential requests. Record
deliberate message differences; assert stable kinds/stages/limits rather than libcurl text.

### P10-03 — add one verification entry point

Add a small cross-platform `xtask`, preferably standard-library-only, that invokes formatting,
compilation, warning-denied linting, default/native/all-feature tests, doctests, and selected stress
filters. Existing packaging/DLL scripts remain specialized leaves. The runner prints exact commands
and fails closed; later it may create source archives and evidence metadata.

### P10-04 — automate without erasing target-host gates

Use that entry point in Windows/Linux CI once a public repository exists. Exact Ubuntu
20.04/Rust 1.85, Wine 5, Windows 10, Schannel ordinary-user context, DLL loading, and GDS canaries
remain target-host gates unless CI genuinely reproduces them.

### P10-05 — run the scheduled long campaign

Run at least one hour of exact-source Ubuntu parser/state-machine thrash while parity work proceeds.
Retain reviewed seeds and any minimized reproducer, not the generated corpus. Longer multi-hour/day
lifecycle soak remains WP11.

## 8. Module organization during WP10

`native_http.rs` is large enough to make review expensive, but extraction is not itself parity.
After parity identifies stable seams, first move its test body without behavior changes. Then
consider separate private framing, pooling, upload-pump, and response-streaming modules in small
mechanical commits. Run the identical gate before and after each move. Never combine extraction
with a parity/lifecycle fix, and never publish internal reactor types merely to make files smaller.

## 9. Consumer-documentation rehearsal

Draft compile-checked guide skeletons during WP10 for blocking use, callbacks, manual driving,
streaming, cancellation, shutdown, DLL/FFI ownership, limits, TLS, and backend selection. These are
contract probes, not WP11 publication polish: awkward examples should reopen an API decision while
native is still private.

## 10. Post-WP11 component direction

Keep one public `nbreq` crate. A future Engine may issue distinct HTTP, DNS resolver, and TCP
connector facades sharing wakeup, cancellation, limits, metrics, and joined shutdown. Do not expose
`NativeReactor`, Hickory/mio types, socket slots, cache entries, or HTTP-pool internals.

The DNS facade needs a portable contract for A/AAAA ordering, search policy, TTL/cache visibility,
cancellation, and errors. The TCP facade needs NBReq-owned bounded streaming handles,
connect/read/write deadlines, half-close and EOF semantics, and cloneable cancellation; it must not
return a raw socket that escapes the owner. Curl may report `Unsupported`. Separate published
DNS/TCP packages and generic TCP pooling are not planned. A future internal workspace split may
remain invisible behind the one public facade.

## 11. First-slice exit

This initial inventory and P10-01 proposal are source-grounded but deliberately stop before code or
default-selection changes. The first implementation slice closes when construction/capability
semantics are reviewed and frozen, silent curl configuration and misleading metrics are resolved,
the shared matrix is enumerated, and no backend/default behavior has changed accidentally.
