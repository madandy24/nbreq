# GDS native NBReq P10-06 evidence

Status: **Windows 10 slice accepted on DMOUSE2; stock-Wine-5 slice remains.** The authenticated
package passed same-package ureq baseline, persisted native activation, an 81-minute health
observation, prompt refresh and shutdown joins, and persisted same-package ureq rollback.

## Accepted package

- Target: DMOUSE2, Windows 10 Pro 22H2, GDS `#C`.
- Staged folder: `C:\adstemp\gds-nbreq-native-x86`.
- Archive: `gds-nbreq-native-x86.zip`.
- Archive SHA-256: `940EDD4971DB975FBD2471CFBAC156C1788CD996DC8090CE06D1CE4F14714355`.
- Archive size: 15,943,934 bytes.
- GDS source: `87cf1098a7ae296004ff63409351aeb3f56c859f`.
- NBReq source: `b3ea96f3f4e7fab4ae3eaecd4c0073a1d03923c5`.
- GDS DLL SHA-256: `05CFFED1281C0E9E7EF33CB53D1099AB1138E1E9016490105468A8906D4FE2DF`.
- Delphi host SHA-256: `E8D1FC47E1336FED558C3FE4CD1C139CE4CB7B009F5BCD009B4D64DB71A7B6D4`.
- PDF font data SHA-256: `7DCECDB17867500E590C2EAEB491E53E5D68CA24D0A41FBB90043307EE487CA0`.
- Wine-5 ProcessPrng shim SHA-256:
  `C199985B0035F332E71CDC597F2568E05FB24B558EC11EDED456732B731EDA0F`.

The package verifier passed locally and again against the DMOUSE2 extraction. It authenticated all
11 manifest entries, required x86 binaries, the native compile marker and public selector exports,
the Delphi runtime data, and the absence of libcurl. Windows used only `gds.exe`, the self-contained
native `gds.dll`, and `PDFFontData.dat`; no Wine shim or curl runtime was present.

## DMOUSE2 drill record

The completed drill followed `gds_native_p10_06_runbook.md`.

| Gate | Result | Evidence |
|---|---|---|
| Extracted package verifier on DMOUSE2 | Pass | 11 hashes, required x86 binaries, native marker/selector exports, no libcurl, and Delphi runtime data verified |
| Same-package ureq baseline | Pass | Exact DLL path and ureq startup marker; both channels and authenticated website traffic healthy |
| Persisted `system_DSHTTPBACKEND=nbreq-native` read back | Pass | Exact value returned while the running process correctly remained on ureq |
| Normal stop and fresh persisted-native start | Pass | Native selected at 18:17:30 without `/nbreqcurlpilottest` |
| Exact native startup policy/path markers | Pass | Exact package DLL, direct-access requirement, explicit GDS no-verify compatibility policy, and completed Rust initialization recorded |
| Primary and backup polling | Pass | Both channels remained active throughout the observation |
| Authenticated login/read and response POST | Pass | Sustained Activity-screen use, ordinary navigation, and a real sound event all succeeded |
| Settings refresh during active long polls | Pass | Both requests cancelled; both pollers joined and recreated; WebRPC Drops completed in about 3 ms |
| 60-minute health interval | Pass | 81 minutes; 738 fetched, 666 responses, and 666 successful POSTs; zero unexpected errors |
| Persisted `system_DSHTTPBACKEND=ureq` read back | Pass | Exact value returned while the running process correctly remained native |
| Normal native shutdown | Pass | Two active polls cancelled; both pollers joined; WebRPC Drops completed in 1–3 ms; owner confirmed no remaining GDS process |
| Same-package ureq restart | Pass | Same DLL selected ureq at 19:40:32; both channels restarted and the board returned green |
| Ureq safe login/read and POST | Pass | 31 fetched, 28 responses, and 28 matching successful POSTs; zero errors |

Every native and rollback `Respond` line mapped to exactly one `Successfully posted: OK` line.
The 72 native and three rollback fetched-only entries were continuing application long polls at the
bounded snapshots, not missing responses. Four native `HTTP request cancelled` lines are the two
expected requests at settings refresh and the two at final shutdown. No other Error line, portable
transport/timeout/limit failure, response loss, duplicate POST, or secret-bearing NBReq diagnostic
appeared.

The ureq refresh retained its known behavior: facade Drop returned immediately while its detached
poll workers completed about 11.5 seconds later. That is rollback-baseline behavior, not attributed
to native. Native synchronously joined its corresponding work in single-digit milliseconds.

This accepts the Windows 10 half of P10-06. It is a correctness and lifecycle canary, not a fleet,
performance, verified-TLS, or ordinary-constructor claim. The complete `wine5-x86` payload must now
repeat activation, live traffic, cancellation/join, normal shutdown, and same-package ureq rollback
on the declared Ubuntu 20.04/stock-Wine-5 host before P10-06 closes. P10-07 remains unauthorized.
