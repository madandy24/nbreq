# GDS curl-pilot integration plan — review draft

Status: planning only. The GDS tree was inspected read-only on 2026-08-17 and has not been changed.
No item in this document authorizes a GDS edit until the plan is reviewed and accepted.

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
- per-call optional timeouts, with DPWebRPC using 25 seconds and GuardLink using 30 seconds;
- UTF-8 text/JSON response consumption and success restricted to HTTP 2xx; and
- long-lived DPWebRPC polling plus retrying outbound POST workers.

No current facade requirement for cookies, decompression, multipart upload, client certificates, or
methods beyond GET/POST was found. Proxy dependence, redirect behavior, and exact on-wire defaults
still require a controlled parity capture rather than an assumption.

## 2. Proposed ownership

Use one explicitly owned NBReq Engine for the GDS HTTP service by default:

```text
DpSysContext (shared application context)
  HttpEngineOwner
    Mutex<Option<nbreq::Engine>>   sole, takeable owner; Engine is never cloned
    Arc<NbreqDpHttpClient>
      nbreq::Client                issued by that Engine

DPWebRPC
  Arc<dyn DpHttpClient>            ordinary facade access
  tracked request handles          poll and POST cancellation, no Client-wide cancel-all
```

The mutex exists because `DpSysContext` is shared while `Engine` is `Send` but intentionally not
`Sync`; it does not make Engine cloneable or hide an Engine inside a Client. Construction remains
`Engine::new(...)` followed by `engine.client()`. Shutdown takes the unique Engine value from the
owner and consumes it.

`DPWebRPC` should stop constructing an independent HTTP implementation. It receives the same facade
from its system context and tracks its own active NBReq request handles. On DPWebRPC stop it cancels
those IDs only. If later evidence shows that one subsystem genuinely needs bulk isolation, give it
a separately and explicitly constructed Engine; do not introduce `Client::cancel_all()` or a hidden
child Engine.

## 3. Adapter and request parity

Add `NbreqDpHttpClient` beside `UreqDpHttpClient`; keep the trait and mocks as the GDS-facing seam.
The curl pilot is compiled behind a GDS Cargo feature, while a runtime setting selects ureq or
NBReq so a deployed pilot can roll back without rebuilding. Feature unification must not silently
select the live implementation.

Before switching any caller, add facade-level controlled-server tests for this matrix:

| GDS operation | Explicit NBReq construction/proof |
|---|---|
| `post_json` / JSON general request | serialized bytes plus explicit `Content-Type: application/json` |
| `post_text` | capture ureq's current wire header/body, then reproduce the accepted content type explicitly |
| form body | explicit percent encoding and `application/x-www-form-urlencoded` |
| GET | no invented body or content type |
| Basic/Bearer auth | explicit header; never included in error text |
| caller headers | byte-for-byte accepted UTF-8 values for the curl pilot |
| HTTP status | only 2xx returned as GDS success; preserve useful redacted status/body diagnostics |
| response text | define and test UTF-8/charset behavior rather than relying on ureq conversion magic |
| redirects | capture current relied-upon behavior and compare with NBReq's conservative table |
| timeout | map the old optional timeout deliberately to NBReq connect/inactivity/total policy |
| no timeout supplied | choose finite pilot defaults because curl DNS/connect cancellation is deadline-bounded |

NBReq must not invent curl's form content type. Every body-bearing GDS path supplies its intended
header through the adapter. This is especially important for GuardLink's token form and JSON API
calls and for DPWebRPC's raw text POST.

Map NBReq's structured failure to the existing `Result<_, String>` only at the GDS facade boundary.
Logs should retain redacted category/stage/timeout detail without curl numbers, URLs containing
query secrets, authorization values, or payloads.

## 4. Cancellable DPWebRPC path

Extend the facade with an internal started-request shape that separates a waitable result from a
cloneable cancellation control. The exact Rust spelling is a review item, but it must allow this
sequence without one network thread per request:

1. DPWebRPC submits its poll and retains the request control.
2. The poller blocks on NBReq's direct waiter, independent of callback workers.
3. `DPWebRPC::Drop` signals shutdown and cancels the retained poll plus any tracked outbound POSTs.
4. The poller and POST pool join synchronously after prompt NBReq cancellation.
5. The existing detached `dpwebrpc-shutdown` workaround is removed only after the bounded join test
   passes through the Delphi entry point.

The ureq implementation remains selectable. Its started-request compatibility implementation may
retain the existing timeout-bounded shutdown behavior; it must not weaken the NBReq path or require
NBReq to emulate ureq's inability to cancel.

## 5. Initialization and shutdown order

Do not initialize curl from `DllMain`, a Rust/Delphi loader callback, or a static constructor. The
GDS HTTP owner is created lazily or through an explicit ordinary entry point after the module is
loaded. Curl initialization remains on NBReq's spawned reactor thread.

Normal GDS shutdown order:

1. stop admission by GDS HTTP-producing subsystems;
2. cancel and join DPWebRPC poll/POST work and other tracked long-lived calls;
3. take the unique Engine from `HttpEngineOwner`;
4. call Engine bulk cancellation and consuming normal shutdown;
5. verify callback/reactor threads have exited; and
6. leave the curl-backed GDS module and preloaded curl DLL pinned until process exit.

Engine restart inside the still-loaded module is supported and tested. `FreeLibrary` unload/reload
of the curl-backed GDS module is not supported and must not be used as a stop mechanism.

## 6. Packaging and rollout work packages

### G0 — Review and freeze

- Accept Engine placement, facade started-request shape, shutdown ordering, setting names, and
  security defaults.
- Resolve the open questions below. No GDS mutation before this gate.

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

## 7. Review questions

1. Where should the runtime backend choice live, and what existing configuration mechanism should
   carry it?
2. Should the curl pilot initially reproduce today's globally insecure Rust behavior for selected
   legacy installs, or is there already a reliable GDS setting that can scope no-verify? The NBReq
   default remains verified either way.
3. What finite connect and total defaults should the adapter apply when current callers pass
   `None`?
4. Does any deployed endpoint rely on environment proxies or ureq redirect behavior?
5. Is strict UTF-8 response decoding acceptable for every current JSON/text caller, or must a
   legacy charset conversion be preserved?
6. For Wine 5, does the canary use explicit no-verify, a newer Wine/trust path, or a separately
   provisioned root? Generated custom trust currently fails through that legacy Schannel.
7. Which Delphi owner performs final process shutdown, and can it formally guarantee that the
   curl-backed GDS DLL is never `FreeLibrary`-unloaded before process exit?
