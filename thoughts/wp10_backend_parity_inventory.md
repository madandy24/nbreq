# WP10 backend parity inventory

Status: P10-01, P10-02, P10-03, and P10-05 accepted, 2026-08-23; P10-04 CI automation is deferred
until a public repository exists. Explicit backend construction, conservative curl pool bounds,
connection-metrics availability, shared black-box parity, the cross-platform verification entry
point, and the scheduled Ubuntu campaign pass their gates. P10-06's explicit native GDS rollout is
accepted; P10-07 now has a Windows implementation checkpoint for native ordinary/default
selection. Exact-source Ubuntu, ordinary-account Windows, and a separate GDS smoke/rollback remain.
This does not alter the accepted GDS package or remove curl/ureq rollback.

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
| Ordinary `Engine::new` | Selects native in the default build; `HttpBackend::Native` is also available explicitly | Available only as explicit `HttpBackend::Curl`; compiling the feature does not change ordinary selection | **P10-07 implemented on Windows.** Default native, feature-unification immunity, and no-default `Unsupported` behavior are public-contract tested; platform acceptance remains. |
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

`tests/http_adversarial.rs` now enumerates every compiled real backend through the public explicit
selector. In an all-feature build each test therefore runs against both native and curl rather than
allowing implicit curl selection to hide native. The shared suite covers fragmented fixed/chunked
responses, malformed status/headers/framing, premature EOF, abortive upload, sequential reuse,
buffered response limits, total/inactivity clocks, individual cancellation, consuming shutdown,
redirect policy, and portable TLS outcomes. The public contract suite separately checks
ownership/thread traits, empty metrics, spawned-drive rejection, and the intentional manual-mode
difference.

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

## 12. P10-02 first shared checkpoint

Commits `a16ca75`, `fcbd0d8`, and `cbc1911` convert the adversarial integration suite from an
implicit either/or backend test into a public-selector matrix over every compiled backend. The
same fixtures now prove:

- sequential HTTP/1.1 reuse;
- response body bytes, response header bytes, and response header count with exact `LimitKind`;
- total and inactivity timeout categories, including peer-observed socket close;
- individual cancellation and consuming shutdown, including canonical `Cancelled` and
  peer-observed socket close;
- conservative redirect behavior: POST 302 remains a response, 303 becomes bodyless GET, 307
  replays the buffered body, hop exhaustion is `Redirect`, same-origin credentials remain, and
  cross-origin credentials are stripped;
- explicit no-verify success and verified unknown-root failure at the TLS stage when the selected
  implementation has TLS support.

The Windows default gate passes 65 units, 7 contracts, and 6 doctests. Native passes 185 units,
11 shared adversarial tests, 7 contracts, and 6 doctests. Curl passes 84 units, the same 11 shared
tests, 7 contracts, and 6 doctests; the integration TLS case conditionally skips when the selected
system curl reports no TLS implementation. Warning-denied all-feature lint, docs, and formatting
pass. The ten non-TLS shared tests also pass in one all-feature process against both implementations.

The all-feature TLS test reaches the already classified vendored-Schannel restricted-token
credential failure in the Codex environment. Keep the assertion and run it under the ordinary
Windows account rather than converting the environment failure into a product allowance.

This first checkpoint did not yet close shared DNS/connect failure classification, request-wire
policy, or permit recovery after errors. The next checkpoint closes those behavioral items. None
of this changes `Engine::new`, Cargo defaults, the accepted GDS package, or curl/ureq rollback.

## 13. P10-02 Windows behavior checkpoint

Commits `2e52eb8`, `9694a99`, and `1ca54c8` add the remaining shared Windows behavior cases:

- a buffered POST proves origin-form path/query serialization, generated Host and Content-Length,
  fragment removal, and no invented Content-Type or Expect;
- a response-limit failure under one-slot admission releases the permit, allows a second request,
  and leaves accepted/failed/completed counters and current/high-water inflight gauges exact;
- a real 404 callback follows the same `Completed(Response)` terminal path;
- the reserved `.invalid` namespace maps an answered negative resolution to `Transport/Dns`;
- a closed loopback endpoint remains in the connect category whether the host reports immediate
  refusal (`Transport/Connect`) or the restricted network layer silently drops it
  (`Timeout/Connect`).

The last case caught a curl classification bug. libcurl may publish `primary_ip` after choosing an
address but before TCP establishment, so it cannot prove that a transfer reached the connected
stage. Curl timeout classification now uses non-zero `connect_time` as that evidence and no longer
mislabels a pre-connect timeout as total.

The complete local matrix now passes 65 default units; 185 native units plus 16 shared tests; 84
curl units plus the same 16 shared tests; all 7 contracts and 6 doctests; warning-denied all-feature
lint; docs; and formatting. One all-feature process passes all 15 non-TLS shared tests against both
implementations. At this checkpoint, ordinary-user vendored Schannel and exact-source Ubuntu were
the remaining P10-02 gates; the following platform close records both.

## 14. P10-02 platform close and acceptance

The owner runs the focused shared TLS case from an ordinary PowerShell session with
`curl-pilot-vendored`; it passes outside the already classified restricted-token Schannel
environment. No product exception or weakened assertion is needed.

Exact source `9ddf850` is archived as `wp10-p10-02-9ddf850.zip`, 504,284 bytes, SHA-256
`C597D5CB38D7F2075612EF7C8AEEE113580FC5F36305ACFD91042D996A6C3067`. The copied hash matches on
Ubuntu 20.04.6 LTS with Rust/Cargo 1.85.0. One fail-fast offline sequence then passes:

- 65 default units, 7 contracts, and 6 doctests;
- 185 native units, 16 shared adversarial tests, 7 contracts, and 6 doctests;
- 85 curl units, 16 shared adversarial tests, 7 contracts, and 6 doctests;
- all 16 shared tests in one all-feature process, exercising both implementations including TLS;
- warning-denied all-feature/all-target lint, all-feature docs, and formatting.

The sequence runs from 03:57:15 through 04:01:28 UTC, exits zero at `STAGE=complete`, and leaves no
proof process. P10-02 is accepted. The ordinary native/default-feature switch remains gated; P10-03
is next.

## 15. P10-03 Windows verification-runner checkpoint

Commit `b6ef1e1` adds one dependency-free Rust entry point:

`cargo run --manifest-path tools/xtask/Cargo.toml -- verify`

The runner checks its own formatting, tests, and warning-denied lint before running NBReq's
formatting, minimal/all-feature compilation, warning-denied all-target lint, ordinary/minimal/
native/curl/all-feature suites, explicit doctests, documentation, and three named native pressure
regressions. It prints and flushes every exact command before execution, reports stage timing, and
stops at the first failure. `--offline`, `--dry-run`, and a positive `--stress-repetitions N` are
explicit options; invalid input fails closed. Specialized curl packaging, DLL lifecycle, Wine,
Windows 10, GDS, and source-archive evidence remain separate target-host leaves.

The restricted Codex Windows token passes the first eleven product stages and then stops at the
three already classified Schannel environment fixtures in the combined all-feature suite. The
owner's ordinary PowerShell run passes all 17 stages in 63.868 seconds, including that combined
suite and the three final pressure filters. This accepts the Windows half and proves the runner
does not hide its environment gate. Exact-source Ubuntu repetition remains before P10-03 closes.

## 16. P10-03 platform close and remaining WP10 sequence

Exact commit `b6ef1e1` is archived as `wp10-p10-03-b6ef1e1.zip`, 509,410 bytes, SHA-256
`E4C96E601661968787D5CC821E6136B0A3F8C8411958EECF19399765E8EF3745`. The copied size and hash
match on Ubuntu 20.04.6 LTS with Rust 1.85.0 and Cargo 1.85.0. From a fresh `/tmp` extraction, the
same offline entry point passes all 17 stages in 310.297 seconds. Its final marker is
`NBReq verification complete: all 17 steps passed`, and a `/proc` executable/working-directory
ownership check reports zero process associated with the proof tree. Together with the 63.868
second ordinary-user Windows result, P10-03 is accepted.

P10-04 is deliberately deferred rather than simulated: the entry point is ready for Windows/Linux
CI, but no public repository exists yet. Exact Ubuntu 20.04/Rust 1.85, Wine 5, Windows 10,
ordinary-user Schannel, DLL loading, and GDS canaries remain target-host gates even after CI exists.
This deferral is a publication/operations task, not a reason to hold the private native rollout.

P10-05 is accepted by the corrected `dbe3ff0` campaign already recorded in section 7: three
timestamped 1,201-second Ubuntu parser/state-machine legs complete without a generated artifact
after the first run found and retained the root-CNAME regression. Longer multi-hour/day live
lifecycle soak remains WP11.

The next contract review must keep two decisions separate:

1. Add an explicitly selected native GDS canary while ordinary NBReq construction remains
   unchanged, preserving both ureq and curl rollback and exercising the existing cancellation,
   restart, Windows 10, and Wine deployment paths.
2. Only after that canary passes, make `native` a Cargo default and make ordinary
   `Engine::new`/unqualified builder construction select native. Feature unification must never
   select curl implicitly, and a no-default-feature build must fail unsupported ordinary network
   construction rather than expose the scaffold as a third public runtime.

Call these P10-06 (explicit native consumer rollout) and P10-07 (ordinary native/default-feature
switch). Neither is authorized merely by closing P10-03.

## 17. P10-06 GDS implementation checkpoint

GDS commits `aac6b85` and `0c0588d` add the first explicitly selected native consumer path without
changing NBReq ordinary construction or GDS's ureq default. The Rust crate now has a default-off
`nbreq-native` feature and constructs `HttpBackend::Native` explicitly through the existing
`NbreqDpHttpClient` facade. One `DpSysContext` still owns the unique Engine; WebRPC still receives
individual cancellable waiters; ureq and curl remain separate rollback choices. Feature presence
alone selects nothing.

The Delphi boundary assigns stable backend code 2 and persisted value `nbreq-native`. Startup
requires both the `nbreq_native_compiled` marker and the public `dphttp_select_backend` export,
and rejects unknown, unavailable, or late selection rather than falling back. The existing
`/nbreqcurlpilottest` override remains curl-only. Native does not enter curl's adjacent-DLL load and
pin checks. Both NBReq adapters retain GDS's explicit no-verify compatibility policy; this does not
change NBReq's verified default.

The dependency lock aligns the GDS facade and NBReq on URL 2.5.8. Because GDS's serial-port graph
keeps the older `quote` family, GDS enables the compatible `zerovec` 0.11.4 `alloc` feature
explicitly instead of forcing an unrelated derive upgrade. Default and native full test runs each
pass every HTTP/integration test and stop only at the same three unrelated `dscat` injected-signal
fixtures (1,046/1,053 passed respectively, with ten ignored). The focused native and curl NBReq
sets each pass 8/8. Offline native and curl checks and formatting pass. The actual
`stable-i686-pc-windows-msvc` native DLL build completes successfully, and Delphi 7 compiles the
host selector with zero errors.

This is an implementation/build checkpoint, not a live canary. P10-06 still requires an
authenticated self-contained native package, Windows 10 and Wine deployment, real gateway
poll/POST traffic, individual in-flight poll cancellation and join, normal process shutdown, and
persisted `DSHTTPBACKEND=ureq` restart rollback. The curl package and its process-lifetime rules
remain untouched. P10-07 remains unauthorized.

GDS commit `87cf109` adds a separate native packager and verifier. From code-clean GDS `87cf109`
and clean NBReq `b3ea96f`, it rebuilt the Delphi host and release x86 native DLL, built the audited
Wine-5 `ProcessPrng` shim, and produced the 15,943,934-byte
`gds-nbreq-native-x86.zip` with SHA-256
`940EDD4971DB975FBD2471CFBAC156C1788CD996DC8090CE06D1CE4F14714355`. The manifest authenticates
11 files. Both platform folders contain the same GDS host/DLL/font data; only Wine adds the shim.
The verifier proves all PE files are x86, both GDS DLLs export the native marker and public selector,
and neither folder contains libcurl. A fresh archive extraction passes the same verifier. Packaging
is therefore closed.

The same authenticated package then passes the Windows 10 activation and rollback drill on
DMOUSE2. Persisted native selection starts the self-contained backend, both channels poll, an
authenticated Activity-screen session and sound event exercise real reads and POSTs, a settings
refresh cancels and joins both long polls in about 3 ms, and 81 minutes produce 738 fetched IDs,
666 responses, and 666 matching successful POSTs with zero unexpected errors. Final native Drop
joins in 1–3 ms and leaves no GDS process. Persisted ureq restart from the identical package then
produces 28 responses and 28 matching successful POSTs with zero errors. Exact hashes and timings
are recorded in `gds_native_p10_06_evidence.md`.

The Windows half of P10-06 is accepted. The following platform close records the required
stock-Ubuntu-20.04/Wine-5 repair and repetition.

## 18. P10-06 stock-Wine close and acceptance

The first exact Wine launch found a platform-specific readiness failure before HTTP initialization:
Mio 1.0.4 could not open its private `\\Device\\Afd\\Mio` object under stock Wine 5. A standalone
32-bit probe reproduced the same error, and the base `\\Device\\Afd` object was also absent. This
is the old-Wine compatibility described by upstream Mio issue 1444, not a relaxation of native
ownership or an excuse to replace the supported target.

NBReq `6c3bde6` keeps Mio as the ordinary Windows and Unix poller and adds a narrow Windows-only
fallback. Only the first socket-registration `NotFound` naming `\\Device\\Afd` changes that poll
owner to documented `WSAPoll`; switching after any successful registration is forbidden. The
fallback uses the same nonblocking sockets and owner state machines and caps waits at 50 ms because
old Wine cannot use Mio's completion-port waker. NBReq proper remains `unsafe_code = "forbid"`;
minimal WinSock FFI is isolated behind a safe API in the unpublished `nbreq-winpoll` workspace
crate. The forced path proves connect/write/read/FIN/cancel on Windows, and the expanded ordinary-
account verification runner passes all 21 stages in 64.583 seconds. A rebuilt x86 probe returns
HTTP 200 with 559 bytes under stock Wine 5.

GDS `7d4d243` adds fail-closed `/httpbackend {ureq|nbreq-curl-pilot|nbreq-native}` process-local
selection without changing persisted settings or the runtime default. Authenticated archive
`BB492B60E100C89B40D0772311C5D7A47D7364F24D3D1BC5BE0D2DC466E37C37` contains GDS `7d4d243`,
NBReq `6c3bde6`, 11 verified files, required x86 exports/runtime data, and no libcurl. Every copied
and extracted hash matches on Ubuntu 20.04.6/stock Wine 5.

An unknown override fails before HTTP initialization. Native then starts twice from the same
package, logs the exact process-local selection and explicit GDS no-verify policy, runs both real
long-poll channels plus authenticated website traffic, cancels active polls, and joins its two
WebRPC Drops in 7/2 ms at refresh and 4/1 ms at normal close. Both exact-name post-close checks find
no process. The identical files then select ureq through the public override, return a green board
and healthy website, and close with ureq's already-known detached-worker behavior; the final process
check is clean. Exact hashes, log lines, and limitations are in `gds_native_p10_06_evidence.md`.

P10-06 is accepted. P10-07 is now the active, separate default-switch gate: a plain
`cargo add nbreq` must compile and select native; curl remains explicit and immune to feature-
unification selection; and a no-default-feature build fails ordinary network construction as
unsupported rather than exposing scaffold as a third runtime. The accepted GDS package and
ureq/curl rollback remain available while that change is reviewed.

## 19. P10-07 native-default implementation checkpoint

Before changing defaults, NBReq `9378e46` closes P10-06's one Wine carry-forward: every clone of
the Mio waker shares a switchable state, and the first-registration AFD fallback drops and disables
that waker before `WSAPoll` takes ownership. Wake calls then become harmless no-ops instead of
posting unread IOCP completions; the existing 50 ms safety bound remains the fallback's progress
mechanism. A regression exercises 20,000 calls through pre-existing clones after the switch.

NBReq `733c294` makes `native` the Cargo default and makes `Engine::new` plus an unqualified builder
select native. Curl is available only through explicit `HttpBackend::Curl`, even when Cargo feature
unification compiles both implementations. A no-default build and a curl-only build both compile;
ordinary construction returns `Unsupported` without native rather than exposing the lifecycle
scaffold. Public-contract tests cover all three cases. The verification runner adds a distinct
curl-only stage, so its ordinary run now contains 22 stages.

Windows passes the 188-unit default-native suite plus 16 shared adversarial tests, 7 public-contract
tests, and 6 doctests. Curl-only passes 83 units plus the same 16 shared tests, 5 contracts, and 6
doctests. Default-native plus curl passes 207 units, 16 shared tests, 9 contracts, and 6 doctests;
warning-denied all-feature lint, documentation, formatting, and minimal-feature compilation pass.
The restricted-token full runner reaches the already-classified three Schannel fixtures; its one
additional streaming-fixture race was a dishonest peer-response gate and is corrected without a
product change. Ordinary-account Windows must still run all 22 stages. Exact-source Ubuntu/Rust
1.85 and one separate GDS native smoke plus same-package ureq rollback remain before acceptance;
neither proof may replace the accepted P10-06 package.
