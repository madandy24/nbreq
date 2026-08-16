# GDS curl-pilot integration plan — G0 freeze candidate

Status: planning only. The GDS tree was inspected read-only on 2026-08-17 and has not been changed.
The ownership, request-cancellation, timeout, and first-canary TLS decisions are frozen here for one
final review. No item in this document authorizes a GDS edit until that review accepts G0.

## 1. Read-only findings

The HTTP facade is concentrated in
`gds/rust/gds/src/dplib/dphttpclient.rs`. `DpHttpClient` exposes blocking JSON POST, text POST, text
GET, and a general GET/POST request. `MockDpHttpClient` is widely used and is the right compatibility
seam to preserve.

There are presently two creation/ownership paths:

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

`DPWebRPC` should stop constructing an independent HTTP implementation. It receives the same facade
from the same `default_sys_context()` value and tracks its own active request controls. On DPWebRPC
stop it cancels those IDs only. If later evidence shows that one subsystem genuinely needs bulk
isolation, give it a separately and explicitly constructed Engine; do not introduce
`Client::cancel_all()` or a hidden child Engine.

## 3. Adapter and request parity

Add `NbreqDpHttpClient` beside `UreqDpHttpClient`; keep the trait and mocks as the GDS-facing seam.
The curl pilot is compiled behind a GDS Cargo feature, while a runtime setting selects ureq or
NBReq so a deployed pilot can roll back without rebuilding. Feature unification must not silently
select the live implementation.

Before switching any caller, add facade-level controlled-server tests for this matrix:

| GDS operation | Explicit NBReq construction/proof |
|---|---|
| `post_json` / JSON general request | serialized bytes plus explicit `Content-Type: application/json` |
| `post_text` | current ureq 2.12 `send_string` adds no Content-Type; deliberately preserve that omission unless the caller supplied one |
| form body | explicit percent encoding and `application/x-www-form-urlencoded` |
| GET | no invented body or content type |
| Basic/Bearer auth | explicit header; never included in error text |
| caller headers | byte-for-byte accepted UTF-8 values for the curl pilot |
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
5. verify callback/reactor threads have exited; and
6. leave the curl-backed GDS module and preloaded curl DLL pinned until process exit.

Engine recreation inside the still-loaded module is supported only after the old service is stopped
and its facade users are rebuilt. It is not transparent: a DPWebRPC instance holds its facade Arc
for life and must be recreated rather than having an Engine swapped beneath it. `dpwebrpc_free`
drops that one instance; it is not `FreeLibrary`. `FreeLibrary` unload/reload of the curl-backed GDS
module is unsupported and must not be used as a stop mechanism.

## 6. Packaging and rollout work packages

### G0 — Review and freeze

- **Freeze candidate:** one atomic context-owned HTTP service state; one unique NBReq
  Engine; context-issued facade; neutral started request; per-DPWebRPC cancellation tracker;
  `Some(t)` preservation; finite 30-second `None`; explicitly insecure first canary; ureq default
  rollback; and consuming Engine shutdown after subsystem joins.
- Final review must accept the runtime setting source/name and confirm the remaining deployment
  questions below. No GDS mutation before this gate.

### G1 — Dependency and selection scaffold

- Add NBReq as a local/path dependency with the controlled curl-pilot feature.
- Add compile-time availability plus explicit runtime `ureq` / `nbreq-curl-pilot` selection.
- Preserve mocks and make ureq the initial/default rollback choice.

### G2 — Wire-compatible adapter

- Implement the blocking adapter and explicit body/header encoding.
- Add ureq-versus-NBReq controlled-server parity tests without sending duplicate production
  mutations.
- Audit every current call site for timeout, content type, auth, redirect, and error assumptions.

### G3 — DPWebRPC cancellation

- Add started requests and per-DPWebRPC handle tracking.
- Prove cancel during long poll, outbound POST, restart/handoff, rapid create/free, and watchdog
  paths.
- Remove detached shutdown only after synchronous bounded stop passes.

### G4 — Exact DLL lifecycle and packaging

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

- Run focused Rust facade/DPWebRPC tests, the Delphi bridge test, and the test gateway.
- Repeat on Windows 10, native Ubuntu 20.04, and the supported Ubuntu 20.04/Wine environment.
- Exercise primary/secondary endpoints, settings refresh, long poll, POST retry, Engine restart, and
  process shutdown.

### G6 — Canary and rollback

- Start with an explicitly selected pilot deployment and redacted stage/timing logs.
- Never dual-send a real mutation. Differential mutation tests use only controlled fixtures.
- Make rollback to ureq a setting change and record the decision/health criteria before canary.

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

Remaining final-review/deployment questions:

1. Which existing GDS configuration source and public setting name carries the explicit backend
   enum? The initial/default value is ureq and rollback must remain a setting change.
2. Does any deployed endpoint rely on environment proxies or ureq redirect behavior?
3. Is strict UTF-8 response decoding acceptable for every current JSON/text caller, or must a
   legacy charset conversion be preserved?
4. For Wine 5 after the first insecure canary, do later deployments use a newer Wine/trust path or
   a separately provisioned root? Generated custom trust currently fails through legacy Schannel.
5. Which Delphi owner performs final process shutdown, and can it formally guarantee that the
   curl-backed GDS DLL is never `FreeLibrary`-unloaded before process exit?
6. What exact Wine-5-compatible preload mechanism will pin the verified curl DLL without falling
   back to ambient search-path selection?
