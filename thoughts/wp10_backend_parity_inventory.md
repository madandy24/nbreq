# WP10 backend parity inventory

Status: P10-01 accepted, 2026-08-23; P10-02 shared black-box parity is active. Explicit backend
construction, conservative curl pool bounds, and connection-metrics availability pass their
Windows and exact-source Ubuntu gates. Ordinary backend selection is deliberately unchanged. This
does not alter the accepted GDS package or remove curl/ureq rollback.

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
| Ordinary `Engine::new` | Does not yet select native; `HttpBackend::Native` is available explicitly when compiled | Still selected implicitly by `curl-pilot`, and also available as explicit `HttpBackend::Curl` | **Partially closed.** Explicit feature-invariant selection now fails unavailable implementations at construction. The separately gated native/default-feature switch remains. |
| Spawned mode | Supported | Supported | **Required parity** for acceptance, wakeup, cancellation, terminal arbitration, panic containment, callbacks, and consuming shutdown. |
| Manual mode | Supported through the same native state machines | Construction returns `WrongMode` | **Explicit curl limitation.** Do not add an unsafe binding wrapper merely for symmetry. |
| Buffered HTTP | Supported | Supported | **Required black-box parity** for request wire policy, responses, redirects, limits, timeout/error kinds, cancellation, and shutdown. |
| Streaming response/upload | Supported with bounded queues and fixed/chunked producer framing | Capability check returns `Unsupported` before acceptance | **Explicit curl limitation.** Required for native readiness, not for the disposable reference backend. |
| Callback `start` family | Uses the backend-neutral registry and dispatcher | Same | **Required lifecycle parity.** User callbacks remain queued off-reactor; streaming callbacks remain absent on both. |
| DNS implementation | Engine-owned Hickory wire service with joined resolver thread | libcurl/platform resolver | **Internal difference.** Portable HTTP results and bounded shutdown matter; a raw DNS facade is post-WP11. |
| TCP implementation | NBReq mio owner with generation-checked slots | libcurl Multi/easy owner | **Internal difference.** Portable cancellation/close behavior matters; a raw TCP facade is post-WP11. |
| TLS | rustls with platform verification; explicit no-verify keeps handshake signatures | pinned libcurl platform TLS; same explicit compatibility policy | **Required policy parity; environment-specific mechanics.** Generated and platform-store fixtures remain named gates. |
| HTTP connection reuse | Bounded NBReq owner pool with no transparent replay | libcurl connection cache | **Required observable safety**, not identical algorithms. Contamination, framing, cancellation, and redirect rules must agree. |
| Active/idle pool settings | All five public settings are enforced | Total/host active limits and a conservative total idle cache are configured; zero/floored-subsecond idle policy disables reuse | **P10-01 accepted on Windows and exact-source Ubuntu.** The curl policy is an upper bound and may retain fewer connections than native. |
| Request lifecycle metrics | Backend-neutral accepted/completed/failed/cancelled and queue gauges | Same registry counters | **Required parity** and shared tests. |
| Connection/pool metrics | Native records opened/reused/closed/evicted and active/idle/waiter gauges and reports availability | Fields remain zero and `connection_metrics_available()` is false | **P10-01 implemented.** Request/lifecycle metrics remain available on every backend; no curl connection activity is invented. |
| Body/header/event limits | Request limits enforced before admission; native enforces response and stream bounds | Request limits are shared; curl receives response body/header bounds | **Required black-box parity**, including `LimitKind` and permit release. Streaming bounds are irrelevant after curl's pre-admission `Unsupported`. |
| Connection limits | Native enforces total/per-origin active and idle bounds | Curl Multi enforces total/host active maxima and `min(global idle, per-origin idle)` cached connections | **P10-01 accepted conservatively.** Peer-visible transition tests prove the active-plus-idle total bound on Windows and exact-source Ubuntu. |
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

The first source audit identified these concrete gaps:

1. There is no explicit public backend-selection type; feature presence selects curl and can
   override a compiled native implementation.
2. Curl silently ignores five public connection-pool settings.
3. Curl reports native connection/pool metrics as plausible zero values with no availability
   signal or documented backend meaning.
4. The shared black-box suite is far narrower than the combined backend-specific redirect,
   timeout, TLS, limit, cancellation, and lifecycle suites.
5. Native is not yet built through an ordinary consumer constructor, so constructor parity itself
   has not been exercised.

P10-01 closes items 1–3 and exercises native construction through the public builder, without
changing `Engine::new`. Item 4 is P10-02. The final ordinary-constructor/default-feature portion of
items 1 and 5 stays behind the named switch gate.

Manual curl, curl streaming, resolver internals, the UTF-8 curl-header
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

## 6. P10-01 accepted construction/capability freeze

This is the accepted public shape. It does not authorize the later default switch:

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
- At that same switch, compile native support for ordinary consumers by making `native` a default
  Cargo feature (or by making its dependencies unconditional). The intended release experience is
  that plain `cargo add nbreq` constructs a working native Engine with no feature selection. A
  default that falls through to the scaffold or `Unsupported` is not an acceptable switch.
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
  a positive subsecond value therefore disables reuse rather than exceeding the caller's bound;
- prove that curl's total Multi connection limit and idle cache overlap cannot retain more peer
  sockets than native would under the same five knobs. Passing zero to `MAXAGE_CONN` is expressly
  forbidden: it is not curl's disable-reuse setting, so a floored subsecond duration must use the
  per-transfer disable-reuse path.

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
This slice adds explicit selection and honest curl bounds/metrics only; it does not change
`Engine::new`. The later named default-switch gate changes both ordinary construction and Cargo's
default compilation together.

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

The first attempt used exact source `95b61a6` and divided the intended hour into three 1,200-second
legs. The buffered decoder completed 5,097,309 executions with 983 coverage points and 3,293
features. The streaming decoder completed 6,593,440 executions with 1,789 coverage points and
5,195 features. Both were clean. The DNS leg then found a real policy invariant violation after
roughly 183,000 executions: a mutated but parseable answer could produce a root CNAME target, which
`parse_answer` returned for resolver follow-up even though the target is not a usable host name.

The retained 163-byte generated input is
`crash-d22ada9e2205d2e60658119d39d69f73a4546323`, SHA-256
`45ABE31A9E8E16D6927CCD7011390D17CABD3DEF1C11688ABCAB77C6F45535C2`. Commit `1f7784a`
rejects the root target before adding it to the accepted-name set, trusting any address beneath it,
or constructing follow-up work. A direct regression and the smaller reviewed
`root-cname.seed` preserve the production branch. The complete native/default/lint gate passes on
Windows. Because the third leg stopped early, this is a successful finding, not a completed
campaign; all three legs must run again on the corrected exact source.

Corrected exact source `dbe3ff0`, archive SHA-256
`A949E9B54B6272930E58396C2925FD29BCFE7EA5C7F47C9F888680C1BF4A4605`, completes the repeat on
Ubuntu 20.04 / Rust and Cargo 1.85. The buffered leg runs 5,585,729 executions with 987 coverage
points and 3,306 features; streaming runs 6,506,947 with 1,792 coverage points and 5,332 features;
DNS runs 2,024,469 with 2,559 coverage points and 6,340 features. Each target runs 1,201 seconds and
produces no generated artifact. Timestamped stages then pass 65 default units, 7 contracts and 6
doctests; 185 native units, 4 adversarial tests, 7 contracts and 6 doctests; 85 curl units, 5
adversarial tests, 7 contracts and 6 doctests; warning-denied all-feature clippy, formatting, and
offline all-feature compilation. The final marker is `EXIT stage=complete rc=0`, and no proof
process remains.

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

## 11. First-slice checkpoint and exit

The first implementation checkpoint adds the feature-invariant `HttpBackend` enum and
`EngineBuilder::http_backend`, public spawned/manual native construction, deterministic
`Unsupported` for unavailable implementations, conservative curl Multi limits, and an explicit
connection-metrics availability bit. The current `Engine::new` and Cargo `default = []` behavior
remain unchanged.

Windows gates pass separately for default, native, and curl, including a peer-visible proof that a
two-connection curl limit is not exceeded while cached connections give way to active transfers.
A unit gate proves a positive subsecond idle timeout takes `forbid_reuse` instead of setting
`MAXAGE_CONN` to zero. Warning-denied all-feature clippy, docs, formatting, and compilation pass.
The combined all-feature test reproduces only the three already classified restricted-token
Schannel fixture failures; the immediately preceding curl-only suite passes all three.

The same public selection/metrics and curl-limit gates pass in the complete exact-source Ubuntu
run recorded above. Review accepted P10-01 and closed the slice. It does not authorize the ordinary
native/default-feature switch; P10-02 shared black-box parity is next.
