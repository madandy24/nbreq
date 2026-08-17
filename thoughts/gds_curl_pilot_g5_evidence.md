# GDS curl-pilot G5 selected-backend evidence

Status: the stock-Wine selected-NBReq live slice passed on 2026-08-17. This is strong G5 evidence,
but G5 remains open for the explicitly listed target/retry/restart remainder below. It is not a
public-setting decision or production-canary authorization; ureq remains the default.

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

## Accepted claim and open remainder

This passes the selected-NBReq stock-Wine GDS live slice: exact authenticated package, explicit
backend selection, primary/backup real long polling, successful real application POST responses,
live settings refresh, prompt request cancellation and WebRPC recreation, sustained interactive
traffic, normal process shutdown, and no leftover process.

It does not by itself close all of G5. Remaining work is deliberately explicit:

- exact selected-GDS live coverage on the declared Windows-10 target, and deciding whether a
  separate native-Ubuntu GDS consumer run is meaningful for this Windows-hosted bridge;
- a controlled exact-host POST retry/failure observation if the existing G3 simultaneous
  poll/POST cancellation test is not accepted as sufficient for the destructive live case;
- the planned Engine/process restart observation beyond WebRPC-only live refresh/recreation;
- deployed proxy/redirect and non-UTF-8 response dependence checks; and
- G6's persisted public setting, restart-based activation procedure, redacted operational logging,
  decision thresholds, and ureq rollback drill.

The G5 process-local selector stays private until those rollout decisions are reviewed.
