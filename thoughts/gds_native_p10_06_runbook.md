# GDS native NBReq P10-06 activation and rollback runbook

Status: implementation/build checkpoint only. Use this for the controlled Windows 10 and Wine
canaries required to accept P10-06. It does not authorize P10-07, a fleet rollout, or a change to
ordinary NBReq construction.

## Setting contract

GDS reads the system DPConfig value `DSHTTPBACKEND` before Rust HTTP initialization. Relevant
values are:

- absent or `ureq`: select the existing ureq facade;
- `nbreq-curl-pilot`: select the retained reference backend when its matching package is present;
  and
- `nbreq-native`: select NBReq's self-contained native backend when compiled into `gds.dll`.

The setting is case-insensitive after surrounding whitespace is removed. Every other non-empty
value is an error. Feature presence alone selects nothing. `/nbreqcurlpilottest` remains a
curl-only proving override and must not be present during a native canary.

The CAT settings API exposes the value as `system_DSHTTPBACKEND`. Read it with:

```json
{"action":"settings_get","items":["system_DSHTTPBACKEND"]}
```

Set native or rollback with:

```json
{"action":"settings_set","items":{"system_DSHTTPBACKEND":"nbreq-native"}}
```

```json
{"action":"settings_set","items":{"system_DSHTTPBACKEND":"ureq"}}
```

A setting change does not hot-swap the live Engine. It takes effect after a normal full-process
stop and fresh start. Missing native feature/marker, missing selector export, unknown selection,
or selection after HTTP initialization fails closed instead of continuing on ureq.

## Package and preconditions

1. Use one authenticated `gds-nbreq-native-x86.zip` package. Verify `manifest.sha256` and run
   `verify_nbreq_native_package.ps1` on Windows before copying binaries.
2. Use `windows-10-x86` on Windows 10. Use the complete `wine5-x86` folder on the declared Ubuntu
   20.04/stock-Wine-5 target; its `bcryptprimitives.dll` is the audited `ProcessPrng` compatibility
   shim. Do not mix platform folders.
3. Confirm the native package contains no `libcurl.dll`. Do not borrow one from the curl package.
4. Preserve the known-good start command, gateway configuration, current setting, and previous
   binaries. Establish a healthy same-package ureq baseline before native activation.
5. Confirm every deployed endpoint is reachable directly. Proxy support remains outside the
   accepted native v1 contract.
6. Acknowledge that the GDS compatibility adapter still explicitly disables certificate and
   hostname verification for this rollout. NBReq's own default remains verified.

## Baseline and activation

1. Stop GDS normally and install one complete native package folder beside `gds.exe`.
2. Start with `DSHTTPBACKEND` absent or set to `ureq`. Require the exact Rust DLL path,
   `HTTP backend selected: ureq`, and `Rust initialization complete` in the log. Exercise both
   WebRPC channels and one authenticated login/read plus response POST.
3. Set `system_DSHTTPBACKEND` to `nbreq-native` and read it back. The running process remains on
   ureq; do not mistake the setting refresh for activation.
4. Close GDS normally, confirm process exit, then start the same package without
   `/nbreqcurlpilottest`.
5. Require these startup markers before application traffic:
   - the exact package Rust DLL path;
   - `HTTP backend selected: NBReq native (DSHTTPBACKEND; direct access required; TLS verification
     disabled by the GDS compatibility adapter)`; and
   - `Rust initialization complete`.
6. Confirm both configured WebRPC channels complete at least two poll cycles without a portable
   transport/timeout/limit failure.
7. Perform one ordinary authenticated login/read workflow and confirm its response POST returns
   `OK`. Do not dual-send or deliberately replay a live mutation.
8. Perform a normal CAT settings refresh while long polls are active. Require both individual
   polls to cancel, join, recreate, and resume. A healthy-target WebRPC Drop above 500 ms is an
   investigation/rollback trigger.
9. Leave normal long polls and interactive traffic running for at least 60 minutes. Record fetched
   IDs, response POST IDs, errors, refresh timings, and final shutdown timing. This is a
   correctness/lifecycle canary, not a performance claim.
10. Close normally and confirm the process exits with no detached NBReq worker or resolver process.

## Health rules

The canary is healthy only while:

- primary and backup polls continue at the established cadence;
- every Delphi `Respond` ID has exactly one successful POST, with any fetched-only ID explained at
  the application layer rather than treated as an allowed loss;
- errors expose portable kind/stage/timeout/limit detail without URLs, authorization, headers, or
  bodies;
- refresh and final shutdown join rather than detach; and
- thread, socket, memory, and responsiveness remain reasonable relative to the ureq baseline.

Rollback immediately for a wrong/missing backend marker, file/hash mismatch, repeated dual-channel
failure against a healthy ureq baseline, unexplained response loss/duplication, cancellation or
shutdown stall, secret-bearing logs, a required proxy/non-UTF-8 response, or process instability.

## Persisted ureq rollback

1. Set `system_DSHTTPBACKEND` to `ureq` and read it back. If CAT is unavailable, use another
   trusted configuration route against the same database.
2. Close the native process normally, recording cancellation/join and process-exit behavior.
3. Restart the same authenticated package; do not replace the DLL merely to select ureq.
4. Require `HTTP backend selected: ureq`, healthy primary/backup polls, and a successful safe
   login/read plus response POST.

P10-06 is accepted only after the exact package hashes and both platform runs are recorded, the
native activation and same-package ureq rollback pass, and the live cancellation/shutdown evidence
meets the bounds above. Until then native stays explicitly selected and P10-07 remains closed.
