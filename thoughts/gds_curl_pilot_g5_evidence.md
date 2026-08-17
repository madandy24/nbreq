# GDS curl-pilot G5 selected-backend evidence

Status: accepted on 2026-08-18. The stock-Wine selected-NBReq live slice, exact selected-GDS
Windows-10 slice, full process-restart slice, controlled real-NBReq POST retry, and deployed-policy
audit all passed or received an explicit disposition. This is not a public-setting decision or
production-canary authorization; ureq remains the default and G6 owns those controls.

## Frozen source and artifact

- GDS commit: `35902c433b2ee0886242bb2404ac5109077f2ef5`;
- NBReq commit: `ced13230d85453a55db6c824fa9a80ca55885421`;
- both repositories recorded `clean: True` in `BUILD-INFO.txt`;
- archive: `target/curl-pilot/gds-nbreq-curl-pilot-g5-35902c4.zip`;
- archive size: 15,908,632 bytes;
- archive SHA-256: `8E2F7FD8BEE7CB42C374405E47C521718DAC926EE4105E80F4C33089C589218D`;
- Delphi GDS host SHA-256:
  `55C46E496F126F123A2FF51DC0A1EC3775270A4F6C1615700A88CCB35381CCE8`;
- GDS DLL SHA-256: `A322A6F0EA96F3E9C64BEF92B70138CC8556423C4A0747E87DD34A6D28352CF0`;
- libcurl 8.21.0 SHA-256: `C9DF3A41B6CBD3230B9BAD63E4FCEAE31667CBA15C9033B544E1500BCD2E0530`;
- packaged `PDFFontData.dat` SHA-256:
  `DE9AA30FD9AF5ECDEDDEE79E4278B092DBFAF00E535AE69BA920F92C3E1B148E`;
- Wine-5 shim SHA-256:
  `925CFA63E7A288F950A1B0E44C65D9C78C37068D354AB5B55446BC4C0760221B`.

Before packaging, the process-local selector passed focused tests in both configurations: an
ordinary build reported that the pilot was unavailable, a pilot build selected NBReq on a fresh
context, and a pilot build rejected selection after HTTP initialization. The Delphi host compiled
successfully with its existing warning/hint baseline. The package was built from the clean detached
GDS tree, passed the local fourteen-file verifier, and exported both
`nbreq_curl_pilot_compiled` and `dphttp_select_nbreq_curl_pilot_for_test`.

The new `/nbreqcurlpilottest` switch is deliberately process-local. Feature presence alone still
selects nothing. When the switch is present, Delphi requires a pilot DLL and the selector export,
calls it before Rust HTTP initialization, and fails startup if selection is unavailable or late.
This is G5 test injection, not the persisted/public G6 setting.

## Target copy and extraction

The owner-selected target was the same Ubuntu 20.04.6 LTS host with stock Wine 5.0 used for G4. The
archive was copied to `/home/ubuntu/gds-nbreq-curl-pilot-g5-35902c4.zip`. On that host:

- the copied size was exactly 15,908,632 bytes;
- the copied archive SHA-256 matched the value above;
- `unzip -t` reported no compressed-data errors;
- extraction used the fresh `/home/ubuntu/nbreq-g5-35902c4` directory; and
- every one of the fourteen `manifest.sha256` entries passed `sha256sum -c`.

The exact packaged `wine5-x86/gds.exe` was launched as the `#C` role with
`/nbreqcurlpilottest` and `/rustdll` naming
`Z:\home\ubuntu\nbreq-g5-35902c4\wine5-x86\gds.dll`. The startup log recorded the exact Rust DLL,
the exact adjacent pinned `libcurl.dll` with `Wine=True`, the process-local NBReq selection, resolved
exports/callbacks, the embedded dictionary, and complete Rust initialization. Wine's existing ODBC
and NTLM-helper diagnostics remained non-fatal.

## Live gateway and cancellation proof

The CAT module initially had `use_rust_webrpc` disabled, so the startup log honestly reported both
primary and backup WebRPC as Delphi. This was useful negative evidence: selecting NBReq did not
silently alter an unrelated consumer setting, and the still-Delphi polls were not counted as NBReq
traffic.

After the owner enabled the existing CAT module setting, the running process refreshed in place and
reported both primary and backup as Rust. At 20:52:13 the Rust log recorded construction and poller
startup for:

- `http://gds.caverock.com/clients/test2/cat/rpc.php`; and
- `http://gds2.caverock.com/clients/test2/cat/rpc.php`.

Both returned empty successful long polls on an approximately 21-second cadence. The process held
two established sockets whose remote addresses matched DNS for the configured primary and backup
hosts. No curl/NBReq type or error code crossed the GDS facade.

At 20:52:43 a subsequent live settings refresh destroyed and recreated both Rust WebRPC instances.
Each in-flight GET completed as cancelled, each poller exited and joined, outbound workers drained,
and the two Drops reported total join times of 9 ms and 2 ms. Both instances were immediately
reconstructed and resumed repeated successful polling. One shutdown-signal send observed that its
poller had already exited; the subsequent join and completion were successful, so this was a benign
completion/cancellation race rather than a stranded worker.

## Real application traffic

An initial browser login was routed inconsistently because another GDS instance was bound to a
conflicting gateway port. The owner corrected that external test configuration. Before and after
the correction, NBReq's own evidence was clean:

- a real `pda_logon` request reached the Rust poller;
- GDS produced a successful login response;
- its outbound response POST completed with body `OK`;
- a following `settings_get` request and response POST also completed with `OK`; and
- after the gateway conflict was removed, the owner remained logged in and exercised the website
  normally.

The continued workload included repeated `analysis_get` calls and application long-poll activity.
The owner observed good responsiveness, but this document makes no performance attribution: the
test GDS and primary gateway share nearby network placement, and a controlled ureq/NBReq comparison
is future work.

## Normal process shutdown

The owner closed `#C` normally after approximately 29 minutes of selected-NBReq operation. At
21:21:31 both active Rust WebRPC instances cancelled their in-flight GET, exited, joined their
poller, waited for outbound workers, and completed Drop in 4 ms and 2 ms. An exact-name process
check then reported no remaining `gds.exe`.

The GDS error file contained only its pre-existing deliberate canary-overrun test entry. No new
HTTP, CAT, shutdown, or process error was recorded. Curl and the Rust module remained pinned until
process exit as required; no `FreeLibrary` unload/reload was attempted.

## Native Windows 10 and process restart

The identical authenticated archive was then deployed to `C:\adstemp\gds_temp` on DMOUSE2, the
same ordinary-user machine independently recorded by the WP2/WP4 proof as Windows 10 Pro 22H2
build 19045.7663. The live `gds.exe`, `gds.dll`, `libcurl.dll`, and `PDFFontData.dat` sizes and
SHA-256 values matched the frozen package exactly. The proving shortcut preserved the normal
working-directory configuration and launched:

```text
/namesuffix #C /server 192.168.0.101 /transport tcp
/nbreqcurlpilottest /rustdll C:\adstemp\gds_temp\gds.dll
```

At 00:27:51 the native host logged `Wine=False`, the exact adjacent pinned curl path, explicit
process-local NBReq selection, resolved exports/callbacks, the embedded dictionary, and complete
Rust initialization. Both CAT gateway channels reported Rust. Primary and backup pollers used the
real `clients/test` endpoints, and a live refresh at 00:28:32 cancelled both in-flight GETs, joined
their pollers in 2 ms and 1 ms, recreated them, and resumed traffic. Before the first close the
Rust path had fetched 47 non-empty real requests and recorded 42 successful response POSTs with no
transport or poller error. Normal close at 00:34:08 cancelled both active GETs, drained outbound
workers, and completed the two Drops in 15 ms and less than 1 ms. The error file still contained
only the deliberate canary-overrun test entry.

The owner then launched the unchanged NBReq shortcut again. At 00:42:03 a fresh process repeated
the native DLL/curl load and NBReq initialization. Both pollers started again; another live refresh
at 00:42:45 cancelled, joined, and recreated them. An attempted browser login during the normal GDS
warm-up did not succeed, matching established non-NBReq behavior. Without touching the executable,
a second login at 00:43:10 succeeded and normal website use continued. Across the restarted run,
166 unique non-empty requests were fetched and 159 unique responses were submitted. The apparent
seven-request gap was reconciled against the CAT and CAT2 logs: every one of the 159 `Respond <id>`
records maps to exactly one successful `OK` POST, with no duplicate, missing, or
response-without-fetch ID. The seven fetched IDs without `Respond` were application `longpoll`
calls. Delphi deliberately retains those calls for a later signal/expiry response; in this run the
browser replaced each held poll before `RespondRPC` was called. They therefore never entered the
HTTP POST path and are not failed NBReq responses. The earlier 47/42 observation was a live mid-run
snapshot and is not used as a final delivery count. No transport or poller error was recorded.
Final normal close at 01:01:27 cancelled both active GETs, joined both pollers, drained
outbound workers, and completed the two Drops in 2 ms and 1 ms. The owner confirmed the application
had closed; a remote exact-process query was unavailable because DMOUSE2 rejected the caller's
remote-management credentials, so no stronger process-list claim is made.

This closes both the declared Windows-10 selected-GDS target slice and the planned full
process-stop/restart/use/stop observation. It also reconfirms the live settings-refresh path on
native Windows without relying on the Wine result.

## Controlled retry and policy close-out

GDS commit `17ad136` adds a non-destructive loopback proof through the real NBReq/curl facade and
the production DPWebRPC retry loop. The controlled server returns HTTP 503 twice and `200 OK` on the
third POST. DPWebRPC makes exactly three attempts, preserves the identical encrypted request body,
and observes both five-second retry waits through its deterministic test clock. The pilot-enabled
DPWebRPC suite passes all 62 tests; the default suite passes all 61 applicable tests. This closes
retry-after-failure without risking a duplicate live gateway mutation. The full pilot-enabled GDS
suite also reached 1,012 passing tests but retained three unrelated database/global-state failures;
no DPWebRPC or HTTP test failed.

The deployed-policy audit produced these dispositions:

- **Proxy:** no GDS HTTP proxy setting or proxy-aware call path exists. Ureq can inherit process
  proxy environment variables while NBReq deliberately disables them. Pilot selection therefore
  requires direct endpoint access; both selected live targets proved it. G6 must make a required
  proxy a preflight rejection rather than silently changing this rule.
- **Redirects:** no GDS-specific redirect policy or observed live gateway redirect exists. NBReq's
  tested conservative redirect table is accepted for the pilot. A deployment requiring different
  redirect semantics is not eligible for selection.
- **Response text:** GDS builds ureq without its optional charset decoder, so the old path did not
  intentionally convert declared legacy character sets; it replaced malformed UTF-8 lossily.
  WebRPC traffic is ASCII/base64 plus `OK`, while the other production facade callers consume
  JSON/form API responses. Strict UTF-8 is accepted as the fail-closed pilot behavior. A discovered
  invalid-byte dependency blocks selection rather than adding implicit conversion.
- **Native Ubuntu GDS:** not applicable to this Windows Delphi consumer. Native Ubuntu remains
  NBReq-library evidence from WP4; stock Wine 5 is the supported Ubuntu-hosted consumer proof.
- **Restart scope:** process stop/restart is the supported pilot activation and rollback boundary.
  Live settings refresh proves WebRPC cancellation/recreation on the existing Engine, not
  in-process HTTP-service Engine replacement. G6 need not promise the latter.
- **Watchdog/handoff:** the adversarial G3 suite remains the acceptance evidence. The live runs
  exercised ordinary two-channel refresh/handoff, but did not manufacture a watchdog fault.

## Accepted claim and G6 boundary

Together the stock-Wine and native-Windows runs pass the selected-NBReq GDS target/lifecycle slice:
exact authenticated package, explicit backend selection, primary/backup real long polling,
successful real application POST responses, live settings refresh, prompt request cancellation and
WebRPC recreation, sustained interactive traffic, normal process shutdown, and a complete native
process restart. The Wine run additionally proved an exact-name post-close process check.

G5 is accepted. It does not authorize a canary. G6 still owns the persisted public setting,
restart-based activation procedure, direct-connect eligibility check, redacted operational logging,
decision thresholds, health observation, and ureq rollback drill. The process-local selector stays
private until those rollout controls are reviewed.
