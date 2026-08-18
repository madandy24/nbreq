# GDS curl-pilot G6 activation and rollback runbook

Status: authenticated DMOUSE2 candidate ready, not yet canary-authorized. The persisted selector,
redacted NBReq error summaries, and clean package are locally proven. Record the remaining live
setting/restart/60-minute-health/rollback drill in `gds_curl_pilot_g6_evidence.md`; it remains the
G6 acceptance gate.

## Setting contract

GDS reads the system DPConfig value `DSHTTPBACKEND` before Rust HTTP initialization. Accepted
values are:

- absent or `ureq`: select the existing ureq facade; and
- `nbreq-curl-pilot`: select NBReq's private curl backend.

The setting is case-insensitive after surrounding whitespace is removed. Every other non-empty
value is an error. Feature presence alone never selects NBReq. The existing
`/nbreqcurlpilottest` switch remains a private process-local proving override and must not be used
for a canary.

The CAT settings API exposes the value as `system_DSHTTPBACKEND`. An administrator with
`system_settings_set` access can read it with:

```json
{"action":"settings_get","items":["system_DSHTTPBACKEND"]}
```

and set it with either:

```json
{"action":"settings_set","items":{"system_DSHTTPBACKEND":"nbreq-curl-pilot"}}
```

or:

```json
{"action":"settings_set","items":{"system_DSHTTPBACKEND":"ureq"}}
```

Changing the value does not hot-swap a live Engine or existing facade Arcs. It takes effect only
after a normal process stop and fresh start. The supported pilot activation and rollback boundary
is therefore the whole GDS process, not in-process Engine replacement and never `FreeLibrary`.

If NBReq is requested but the feature, adjacent curl, public selector export, or early-selection
condition is missing, Rust initialization fails closed. GDS does not quietly continue as ureq or
Delphi. Recovery is to set `DSHTTPBACKEND=ureq` through another trusted configuration path and
restart.

## Preconditions

Before changing the setting:

1. Use an authenticated GDS/curl-pilot package whose manifest and live file hashes pass.
2. Confirm the target is Windows 10 or the supported Ubuntu 20.04/stock-Wine-5 environment.
3. Confirm the deployment reaches every configured HTTP endpoint directly. This pilot does not
   inherit environment proxy settings.
4. Confirm curl and the GDS Rust module will remain pinned until process exit. Do not plan an
   unload/reload rollback.
5. Record the current `system_DSHTTPBACKEND` value and preserve the known-good start command.
6. Record the current primary/backup gateway configuration and establish that the ureq baseline is
   healthy before attributing a later endpoint failure to NBReq.
7. Acknowledge the first-canary TLS policy: the GDS compatibility adapter still disables
   certificate and hostname verification. This preserves current behavior; it is not NBReq's
   library default or the desired final security state.

## Activation procedure

1. Stop GDS normally, install the authenticated pilot package, and start it with
   `DSHTTPBACKEND` absent or set to `ureq` and without `/nbreqcurlpilottest`.
2. Confirm the log reports the exact Rust DLL, exact adjacent pinned curl DLL, and
   `HTTP backend selected: ureq`. This proves the rollback backend in the candidate package.
3. Set `system_DSHTTPBACKEND` to `nbreq-curl-pilot` and read it back. Do not infer activation from
   the successful setting call; the current process remains on ureq.
4. Close GDS normally and confirm the process exits. Do not replace or unload either pinned module
   inside that process.
5. Start the same package normally, again without the private test switch.
6. Require all of these startup markers before sending application traffic:
   - exact Rust DLL path;
   - exact adjacent pinned curl path and the expected Wine flag;
   - `HTTP backend selected: NBReq curl pilot (DSHTTPBACKEND; direct access required; TLS
     verification disabled by the GDS compatibility adapter)`; and
   - `Rust initialization complete`.
7. Confirm both configured CAT WebRPC channels report Rust, complete at least two poll cycles each,
   and show no NBReq transport/timeout/limit error.
8. Perform one ordinary authenticated login/read workflow and confirm its response POST returns
   `OK`. Do not dual-send or deliberately retry a live mutation.
9. Perform one normal CAT settings refresh. Confirm both in-flight polls cancel, join, recreate,
   and resume. A healthy-target WebRPC Drop above 500 ms is an investigation/rollback trigger; the
   prior target evidence is 15 ms or better.
10. Observe for at least 60 minutes including normal interactive traffic. This is an initial
    canary gate, not a performance comparison or production-release soak.

## Health and logging rules

The canary is healthy only while:

- primary and backup polling continue at their established cadence;
- each actual Delphi `Respond` ID has exactly one successful POST and no response-only ID appears;
- isolated external failures remain classified as `kind`, plus portable `stage`, `timeout`, or
  `limit` detail where available;
- logs contain no request URL/query, authorization value, header value, or request/response body;
- refresh cancellation and final shutdown join rather than detach; and
- process/thread/resource behavior remains stable relative to the ureq baseline.

The seven fetched-only calls in the Windows G5 evidence are not a permitted loss allowance. They
were specifically reconciled as held application long-polls for which Delphi never invoked
`RespondRPC`. Future count differences must receive the same ID-level explanation.

Rollback immediately if the backend marker is absent or wrong, a module path/hash check fails,
both gateway channels repeatedly fail while the ureq baseline is healthy, a response is duplicated
or lost in the HTTP path, cancellation/join stalls, request data appears in logs, a required proxy
or non-UTF-8 response is discovered, or the process becomes unstable.

## Rollback drill

1. While the canary is still reachable, set `system_DSHTTPBACKEND` to `ureq` and read it back. If
   the CAT path is unavailable, use another trusted GDS/configuration route against the same
   database; do not depend on the failed HTTP Engine to repair itself.
2. Close the selected-NBReq process normally. Record WebRPC cancellation/join times and confirm the
   process exits.
3. Restart the same authenticated package. No DLL replacement is required: the pilot build still
   contains the ureq facade.
4. Require `HTTP backend selected: ureq` and successful primary/backup traffic. The curl DLL may
   still be loaded by the pilot build; rollback means ureq handles requests, not that a pinned
   dependency is unloaded mid-process.
5. Repeat the safe login/read check and confirm its response POST.

G6 is accepted only after both activation and rollback are observed on the chosen canary target,
the evidence records exact package hashes and relevant log times, and the operator agrees that the
health/rollback thresholds above are usable.
