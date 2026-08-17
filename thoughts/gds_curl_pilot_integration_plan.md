# GDS curl-pilot integration plan — G5 accepted; G6 next

Status: G0-G4 were accepted on 2026-08-17 behind an internal, default-off `nbreq-curl-pilot`
feature. Ureq remains the runtime default. The final clean x86 package passed native and stock-Wine-5
exact-host load/pin proofs. G5 selected-NBReq live slices now pass stock Wine 5 and the declared
Windows-10 target, including primary/backup gateway traffic, live refresh/cancellation/recreation,
real response POSTs, sustained interactive use, normal shutdown, and a full native process restart.
A controlled real-NBReq retry fixture and source/deployment-policy audit close the bounded G5
remainder. G6's public setting, activation procedure, health criteria, and rollback drill remain
open rather than being inferred from these successful slices.

## 1. G0 read-only findings (historical baseline)

This section records the tree as inspected for G0. It is intentionally historical; the G1–G3
implementation status under section 6 supersedes these observations where the code has now moved.

The HTTP facade is concentrated in
`gds/rust/gds/src/dplib/dphttpclient.rs`. `DpHttpClient` exposes blocking JSON POST, text POST, text
GET, and a general GET/POST request. `MockDpHttpClient` is widely used and is the right compatibility
seam to preserve.

At G0 acceptance there were two creation/ownership paths:

1. `DpSysContext` lazily stores `Arc<dyn DpHttpClient>` and creates the ureq implementation from
   `ensure_http_client()`.
2. `DPWebRPC::new()` calls `create_http_client()` directly and keeps its own trait object, bypassing
   the system-context slot.

`DpSysContext::shutdown()` currently stops its timer manager but does not own or stop HTTP. The
process default context is held in a `OnceLock<Arc<DpSysContext>>`. `DPWebRPC::Drop` sets a flag and
signals its poller, but the blocking ureq GET cannot be interrupted; it therefore spawns a detached
shutdown waiter to join the poller and outbound POST pool after the call eventually returns.

The current Rust HTTP constructor always builds `build_insecure_agent()`. Within this facade,
certificate and hostname verification are unconditionally bypassed rather than selected by an
observed per-request or runtime option. This is probably the remembered old-install compatibility
behavior, but its intended deployment/configuration scope must be confirmed before changing it.

Observed request requirements are:

- GET and POST only;
- buffered JSON, text, and URL-encoded form bodies;
- Basic and Bearer authorization headers;
- caller-supplied headers;
- per-call optional timeouts: observed values include 10, 20, 25, and 30 seconds, while the
  attachment `file_get` path is the one observed caller passing `None`;
- UTF-8 text/JSON response consumption and success restricted to HTTP 2xx; and
- long-lived DPWebRPC polling plus retrying outbound POST workers.

No current facade requirement for cookies, decompression, multipart upload, client certificates, or
methods beyond GET/POST was found. Proxy dependence, redirect behavior, and exact on-wire defaults
still require a controlled parity capture rather than an assumption.

## 2. Proposed ownership

Use one explicitly owned NBReq Engine for the GDS HTTP service by default. Keep the selected facade
and its optional Engine in one mutex-protected lifecycle state so initialization, mock injection,
shutdown, and recreation cannot observe half-installed ownership:

```text
DpSysContext (shared application context)
  Mutex<HttpServiceState>
    Uninitialized
    Ready
      Arc<dyn DpHttpClient>         selected ureq/mock/NBReq facade
      Option<nbreq::Engine>         Some only for NBReq; sole, takeable owner
    Stopped

DPWebRPC
  Arc<dyn DpHttpClient>            ordinary facade access
  WebRpcRequestTracker
    tracked cancellation controls  poll and POST cancellation, no Client-wide cancel-all
```

The mutex exists because `DpSysContext` is shared while `Engine` is `Send` but intentionally not
`Sync`; it does not make Engine cloneable or hide an Engine inside a Client. Construction remains
`Engine::new(...)` followed by `engine.client()`. Shutdown takes the unique Engine value from the
state and consumes it. Existing facade Arcs then contain stopped Clients and fail new work; they do
not recreate an Engine behind the owner.

`Uninitialized` may atomically construct or inject exactly the configured facade and, when selected,
its Engine. `Stopped` rejects facade acquisition, injection, and every new request for ureq, mocks,
and NBReq alike; it never lazily reinitializes and never falls back to a private second client.
Consequently DPWebRPC construction obtains the context facade (initializing it if permitted) or
fails when the HTTP service is stopped.

`DPWebRPC` should stop constructing an independent HTTP implementation. It receives the same facade
from the same `default_sys_context()` value and tracks its own active request controls. On DPWebRPC
stop it cancels those IDs only. If later evidence shows that one subsystem genuinely needs bulk
isolation, give it a separately and explicitly constructed Engine; do not introduce
`Client::cancel_all()` or a hidden child Engine.

## 3. Adapter and request parity

Add `NbreqDpHttpClient` beside `UreqDpHttpClient`; keep the trait and mocks as the GDS-facing seam.
The curl pilot is compiled behind a GDS Cargo feature. G1 uses an internal explicit
`ureq` / `nbreq-curl-pilot` enum plus constructor/test injection, defaulting to ureq; feature
unification must not silently select the live implementation. G6 chooses and wires the persisted
public setting before the first canary.

Before switching any caller, add facade-level controlled-server tests for this matrix:

| GDS operation | Explicit NBReq construction/proof |
|---|---|
| `post_json` / JSON general request | serialized bytes plus explicit `Content-Type: application/json` |
| `post_text` | current ureq 2.12 `send_string` adds no Content-Type; deliberately preserve that omission unless the caller supplied one |
| form body | explicit percent encoding and `application/x-www-form-urlencoded` |
| GET | no invented body or content type |
| Basic/Bearer auth | explicit header; never included in error text |
| caller headers | byte-for-byte accepted UTF-8 values for the curl pilot |
| legacy/non-UTF-8 caller headers | audit Delphi-originated/ANSI conversions and prove that the curl pilot returns its documented `Unsupported` result rather than silently changing bytes; any deployed dependency blocks canary selection |
| HTTP status | only 2xx returned as GDS success; current ureq normally converts 4xx/5xx to `Error::Status` before the facade's response-body branch, so capture and preserve the relied-upon error shape rather than assuming body inclusion |
| response text | define and test UTF-8/charset behavior rather than relying on ureq conversion magic |
| redirects | capture current relied-upon behavior and compare with NBReq's conservative table |
| timeout | map every `Some(t)` to both NBReq total timeout and connect timeout `t`; do not round 10/20/25-second callers up to 30 seconds |
| no timeout supplied | apply a 30-second total and connect timeout because curl DNS/connect cancellation is deadline-bounded |

NBReq must not inherit curl's invented form content type. The adapter explicitly supplies
`application/json` and `application/x-www-form-urlencoded`; its raw text path deliberately omits
Content-Type to match current ureq unless the caller supplied one. This is especially important for
GuardLink's token form and JSON API calls and for DPWebRPC's raw text POST.

Map NBReq's structured failure to the existing `Result<_, String>` only at the GDS facade boundary.
Logs should retain redacted category/stage/timeout detail without curl numbers, URLs containing
query secrets, authorization values, or payloads.

## 4. Cancellable DPWebRPC path

Extend the facade with a GDS-owned internal started-request shape that contains no public NBReq
types. It separates a single waitable result from a cloneable cancellation control and allows this
sequence without one network thread per request:

The GDS adapter uses NBReq's direct waiter form and installs no NBReq user callbacks. Callback
prompt-return requirements therefore do not participate in GDS shutdown; the core callback API and
its independent lifecycle guarantees remain unchanged.

1. The tracker acquires a start-activation permit unless its stopping barrier is already closed.
2. DPWebRPC submits its poll, registers the cloneable cancellation control, then releases the
   activation permit. The poller owns the one waiter.
3. The poller blocks on NBReq's direct waiter, independent of callback workers.
4. `DPWebRPC::Drop` closes tracker admission, waits for any start activation to register or fail,
   takes the poll and POST controls under lock, releases the lock, and cancels them individually.
5. If shutdown raced after NBReq acceptance but before registration, registration sees the closed
   barrier and cancels the request immediately. Completion versus cancellation remains harmless.
6. The poller and POST pool join synchronously after prompt NBReq cancellation.
7. The existing detached `dpwebrpc-shutdown` workaround is removed for the NBReq path only after the
   bounded join test passes through `dpwebrpc_free`.

The ureq implementation remains selectable. Its neutral cancellation control reports prompt
cancellation as unsupported and its waiter retains the existing timeout-bounded blocking behavior.
The ureq rollback path may retain the detached shutdown waiter; it must not weaken the NBReq path or
require NBReq to emulate ureq's inability to cancel.

DPWebRPC never calls Engine `cancel_all()`: the context Engine is shared with unrelated GuardLink,
pager, GDS API, and extension requests. Engine-wide cancellation occurs later during complete
`DpSysContext` shutdown after HTTP-producing subsystems have stopped.

## 5. Initialization and shutdown order

Do not initialize curl from `DllMain`, a Rust/Delphi loader callback, or a static constructor. The
GDS HTTP owner is created lazily or through an explicit ordinary entry point after the module is
loaded. Curl initialization remains on NBReq's spawned reactor thread.

Normal GDS shutdown order:

1. stop admission by GDS HTTP-producing subsystems;
2. cancel and join DPWebRPC poll/POST work and other tracked long-lived calls;
3. mark the HTTP service stopped and take the unique Engine from `HttpServiceState`;
4. call Engine bulk cancellation and consuming normal shutdown;
5. verify the reactor (and any future callback workers, if GDS later adopts callbacks) has exited;
   and
6. leave the curl-backed GDS module and preloaded curl DLL pinned until process exit.

Engine recreation inside the still-loaded module is supported only after the old service is stopped
and its facade users are rebuilt. It is not transparent: a DPWebRPC instance holds its facade Arc
for life and must be recreated rather than having an Engine swapped beneath it. `dpwebrpc_free`
drops that one instance; it is not `FreeLibrary`. `FreeLibrary` unload/reload of the curl-backed GDS
module is unsupported and must not be used as a stop mechanism.

Backend selection is fixed for that HTTP-service lifetime. A persisted ureq↔NBReq change takes
effect only through an explicit full HTTP-service recreation or process restart that also rebuilds
DPWebRPC and other facade holders. G6 must choose the supported activation route and put it in the
operator rollback procedure; changing a setting never hot-swaps an Engine beneath existing Arcs.

## 6. Packaging and rollout work packages

### G0 — Review and freeze

- **Accepted:** one atomic context-owned HTTP service state; one unique NBReq
  Engine; context-issued facade; neutral started request; per-DPWebRPC cancellation tracker;
  `Some(t)` preservation; finite 30-second `None`; explicitly insecure first canary; ureq default
  rollback; and consuming Engine shutdown after subsystem joins.
- G1–G3 are authorized. Persisted setting naming is a G6 canary gate rather than a scaffold blocker.

### G1 — Dependency and selection scaffold

**Implemented.** The GDS crate has an optional local NBReq dependency and internal backend enum.
`DpSysContext` owns one mutex-protected HTTP-service state containing the issued facade and unique
Engine. Context-issued facades share a stopped-admission gate, so an Arc obtained before shutdown
rejects fresh ureq, mock, or NBReq work after shutdown begins. Ureq remains the default and no
persisted setting was invented.

- Add NBReq as a local/path dependency with the controlled curl-pilot feature.
- Add compile-time availability plus the internal explicit `ureq` / `nbreq-curl-pilot` enum and
  constructor/test injection; do not invent a persisted or Delphi setting here.
- Preserve mocks and make ureq the initial/default rollback choice.
- Make `Stopped` reject all new work regardless of the selected real or mock facade.

### G2 — Wire-compatible adapter

**Implemented for the frozen pilot surface.** The adapter maps GET/POST, JSON, raw text, form data,
Basic/caller authorization headers, caller headers, exact `Some(t)` and finite 30-second `None`
timeouts, strict UTF-8 response text, 2xx success, and the existing explicit insecure GDS policy.
Controlled loopback comparisons prove form encoding, JSON bytes/content type, raw-text omission of
content type, Basic auth, and caller headers for ureq and NBReq. Source audit found no raw byte-header
path around Rust `String`. G5 accepted direct-connect-only pilot eligibility, NBReq's conservative
redirect policy, and strict UTF-8 after confirming that this ureq build has no charset decoder and
that production callers consume WebRPC ASCII/base64 or JSON/form API responses.

- Implement the blocking adapter and explicit body/header encoding.
- Add ureq-versus-NBReq controlled-server parity tests without sending duplicate production
  mutations.
- Audit every current call site for timeout, content type, auth, redirect, error, response charset,
  and Delphi-originated/non-UTF-8 header assumptions.

### G3 — DPWebRPC cancellation

**Implemented at the Rust seam.** DPWebRPC now obtains the context facade, starts neutral waiters,
tracks individual cancellation controls behind a shutdown activation barrier, and synchronously
joins prompt-cancellable poll/POST workers. Ureq and ordinary mocks retain the detached compatibility
fallback. Tests cover shutdown racing request activation, simultaneous long-poll and outbound-POST
cancellation, rapid create/free, and the existing restart/handoff/poller-recovery suite. The exact
curl-backed DLL and Delphi bridge remain G4/G5 proof rather than a unit-test claim.

- Add started requests and per-DPWebRPC handle tracking.
- Prove cancel during long poll, outbound POST, restart/handoff, rapid create/free, and watchdog
  paths.
- Remove detached shutdown only after synchronous bounded stop passes.

### G4 — Exact DLL lifecycle and packaging

**Accepted.** GDS commit `51269a0` and NBReq commit `81d5fd9` produce a clean, self-verifying x86
package containing separate native-Windows and Wine-5 folders. Both include the exact clean Delphi
proof host and required adjacent `PDFFontData.dat`; the latter adds only the audited `ProcessPrng`
shim. The package records both clean source commits, hashes every payload, checks every shipped
executable and DLL is x86 PE, includes the curl license and incremental dependency notice, and
carries a Windows verifier plus a portable ZIP/LF manifest for direct Ubuntu `sha256sum`
verification.

The Delphi host resolves the configured Rust DLL to an absolute path. Native Windows uses
`LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`; stock Wine uses the absolute
`LOAD_WITH_ALTERED_SEARCH_PATH` compatibility route. The loaded Rust module is path-verified. A
feature-marker export then distinguishes pilot DLLs from ordinary/older ureq DLLs: only pilot DLLs
must have resolved the exact adjacent `libcurl.dll`, after which the host takes a separate retained
curl reference. `TdsRustInterface` owns both handles and never calls `FreeLibrary`, so both remain
pinned until process exit. A real Delphi GDS process loaded the final binary set and logged both
exact paths on native Windows. The complete final archive then passed all fourteen hashes on Ubuntu
20.04.6/stock Wine 5.0, started the `#C` host normally with bundled font data, logged `Wine=True`,
and left no GDS process after owner termination. The native machine is Windows NT 10.0.26200, so the
declared Windows-10 target behavior remains G5 evidence. See `gds_curl_pilot_g4_evidence.md`.

- Build the exact GDS artifact with the pinned curl DLL and audited Wine-5 `ProcessPrng` shim only
  where required.
- Preload curl by verified absolute path and reject ambient-PATH substitution.
- Do not assume the private Windows proof host's exact-path `LoadLibraryExW` strategy works under
  stock Wine 5: its extended-path load fails there even though adjacent-DLL NBReq execution passes.
  Prove a Wine-compatible controlled preload/pin route using the exact GDS host/package, without
  weakening the native Windows absolute-path check.
- Pin curl and GDS modules until process exit; document this in the Delphi host/package procedure.
- Include dependency notices and the curl security-update checklist.

### G5 — Target and live verification

**Accepted.** Clean GDS `35902c4` and NBReq
`ced1323` produced the 15,908,632-byte authenticated archive whose SHA-256 is
`8E2F7FD8BEE7CB42C374405E47C521718DAC926EE4105E80F4C33089C589218D`. The copied Ubuntu archive and
all fourteen extracted payloads verified. The exact `#C` host selected NBReq only through the
process-local `/nbreqcurlpilottest` switch, loaded/pinned its exact adjacent DLLs under stock Wine
5, and then changed both CAT gateway channels from Delphi to Rust through the existing live module
setting refresh.

Primary and backup Rust pollers completed repeated real long polls. A second live refresh cancelled
and joined the two in-flight GET paths in 9 ms and 2 ms, recreated both WebRPC instances, and resumed
polling. Real login/settings traffic arrived through the Rust poller and both response POSTs returned
`OK`; sustained website traffic followed. Normal owner close later cancelled and joined both active
pollers in 4 ms and 2 ms and left no `gds.exe`. The observed responsiveness is not attributed to
NBReq because the test host and gateway placement were not controlled. Full evidence and the
remaining claim boundary are in `gds_curl_pilot_g5_evidence.md`.

The same authenticated package then passed on the declared Windows-10 target. All four live native
files matched their frozen hashes; `Wine=False`, exact adjacent curl, and process-local NBReq
selection were logged. Both real gateway channels repeatedly polled and POSTed successfully. Live
refresh cancelled/joined/recreated both pollers, first normal close joined them in 15 ms and less
than 1 ms, and a fresh process then loaded NBReq again, handled a successful login plus sustained
traffic, and finally joined both pollers in 2 ms and 1 ms. No transport/poller error appeared. The
restarted-run count is reconciled: all 159 `Respond` IDs produced one `OK` POST, while the seven
fetched requests without a response were deliberately held application long-polls replaced before
Delphi called `RespondRPC`. This closes the Windows-10 selected-GDS and process-restart items without
claiming in-process Engine replacement.

GDS `17ad136` closes retry-after-failure through a controlled real NBReq/curl fixture: two HTTP 503
responses are followed by `200 OK`, with exactly three identical encrypted POST bodies and the two
production retry waits. No live mutation was duplicated. The G5 audit also accepts direct access as
a pilot eligibility requirement, the conservative NBReq redirect table, and strict UTF-8. Ureq was
built without charset conversion; the current production surface is WebRPC ASCII/base64 plus JSON
and form APIs. Native Ubuntu GDS is not applicable to the Windows Delphi consumer; WP4's native
Ubuntu library proof and G5's stock-Wine consumer proof remain distinct. Watchdog fault injection
and handoff remain unit/adversarial-suite claims rather than invented live failures.

- Run focused Rust facade/DPWebRPC tests, the Delphi bridge test, and the test gateway.
- Repeat on Windows 10 and the supported Ubuntu 20.04/Wine environment; native Ubuntu is an NBReq
  library gate, not a GDS Delphi-consumer target.
- Exercise primary/secondary endpoints, settings refresh, long poll, POST retry, Engine restart, and
  process shutdown.

### G6 — Canary and rollback

- Start with an explicitly selected pilot deployment and redacted stage/timing logs.
- Never dual-send a real mutation. Differential mutation tests use only controlled fixtures.
- Choose the existing configuration source and public backend-setting name before canary.
- Make rollback to ureq a setting change followed by the documented HTTP-service recreation or
  process restart; record activation steps and decision/health criteria before canary.

## 7. Frozen decisions and remaining review questions

Frozen for the first canary:

- The context owns one Engine and issues the shared facade; DPWebRPC no longer creates a second
  live HTTP implementation.
- Ureq remains the default rollback backend. An explicit internal backend enum distinguishes
  `ureq` from `nbreq-curl-pilot`; Cargo feature unification never selects live behavior by itself.
- Every `Some(t)` maps to total and connect timeout `t`; `None` maps to 30 seconds for both.
- The first selected NBReq/GDS canary explicitly skips certificate and hostname verification to
  reproduce current GDS behavior. NBReq's library default remains verified.
- DPWebRPC uses individual neutral cancellation controls; context shutdown alone uses Engine-wide
  cancellation.
- Existing DPWebRPC instances are recreated across Engine recreation. `dpwebrpc_free` is object
  Drop, while module pinning is a separate Delphi-host/package obligation.
- `Stopped` rejects facade acquisition and new work through ureq, mocks, and NBReq; it never creates
  a fallback implementation. GDS uses direct waiters and installs no NBReq callbacks.

Remaining deployment question:

1. For Wine 5 after the first insecure canary, do later deployments use a newer Wine/trust path or
   a separately provisioned root? Generated custom trust currently fails through legacy Schannel.
