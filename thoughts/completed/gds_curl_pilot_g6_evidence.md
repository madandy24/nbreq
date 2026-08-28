# GDS curl-pilot G6 evidence

Status: **G6 accepted on DMOUSE2.** The authenticated package passed same-package ureq baseline,
persisted NBReq activation, an 81-minute health observation, and persisted ureq rollback.

## Accepted package

- Target: DMOUSE2, Windows 10 Pro 22H2, GDS `#C`.
- Archive: `gds-nbreq-curl-pilot-g6-fce3edc-d6d83e3-x86.zip`.
- Archive SHA-256: `2637572D8B83D5DC0AC78C6504C0E5A72ABD60D29F62C0BD7678A5EF4DF0E6F2`.
- Archive size: 15,919,849 bytes.
- GDS source: `fce3edcd5102f7c9c456082c4d715b4b56c5becd`.
- NBReq source: `d6d83e3752684d8a40bc88b7687ee9c61ac06547`.
- Delphi host SHA-256: `CE209A50988FC5F2C13E8D1893AF709BBC49E7B2175DA9790D8F53B16A7C5883`.
- GDS DLL SHA-256: `D2AB1009A322203AE73FB9DB9706B26F12162586438F0074F8DAC7EE7152CEF2`.
- libcurl SHA-256: `C9DF3A41B6CBD3230B9BAD63E4FCEAE31667CBA15C9033B544E1500BCD2E0530`.
- `PDFFontData.dat` SHA-256:
  `DE9AA30FD9AF5ECDEDDEE79E4278B092DBFAF00E535AE69BA920F92C3E1B148E`.

The build used the established i686 Delphi host and Rust release profiles. Delphi completed with
zero errors and its established 578-warning/1,746-hint baseline; Rust completed with zero errors.
The package was verified before archiving and again after copying, extracting, and hashing the ZIP.
Verifier passes before archiving, after local copied extraction, and against the extracted DMOUSE2
UNC files authenticated all 15 manifest entries, all required x86 binaries, both copies of the
runtime data, and the exact `dphttp_select_backend` export in each packaged `gds.dll`.

The operator package is at
`C:\User\projects\nbreq\target\curl-pilot\gds-nbreq-curl-pilot-g6-fce3edc-d6d83e3-x86.zip`.
The Windows payload is the complete `windows-10-x86` directory; do not mix it with the Wine
variant. The archive also carries the verifier, notices, build provenance, and G6 runbook.

The same archive is staged and extracted on DMOUSE2 at
`C:\adstemp\nbreq-g6-fce3edc-d6d83e3`; the verifier passed against those remote files. An earlier
package exposed a narrow readback-policy omission; GDS `fce3edc` permits an account allowed to
write system settings to read this selector only, and the corrected package was used for all
accepted observations.

## DMOUSE2 drill record

The completed drill followed `gds_curl_pilot_g6_runbook.md`.

| Gate | Result | Evidence |
|---|---|---|
| Extracted package verifier on DMOUSE2 | Pass | 15 hashes, x86 payload/runtime data, and selector export verified over the staged share |
| Live files match the four Windows hashes above | Pass | Exact final files matched |
| Same-package ureq baseline and both channels healthy | Pass | Started 13:47:18; authenticated website use and successful POSTs |
| Persisted `system_DSHTTPBACKEND=nbreq-curl-pilot` read back | Pass | Exact value returned before restart |
| Normal stop and fresh start without private selector | Pass | NBReq selected at 13:56:16 by persisted setting |
| Exact NBReq startup policy/path markers | Pass | Exact GDS/curl paths, `Wine=False`, and insecure compatibility policy recorded |
| Primary and backup complete at least two polls each | Pass | Both remained active throughout |
| Safe authenticated login/read plus `OK` response POST | Pass | Sustained interactive website use passed |
| Settings refresh cancels/joins/recreates both pollers | Pass | 2/3 ms shutdown and 6/5 ms worker joins at 13:56:55 |
| 60-minute health interval | Pass | 81 minutes: 452 empty polls, 426 fetched, 390 responses and 390 matching successful POSTs; zero unexpected errors |
| Persisted `system_DSHTTPBACKEND=ureq` read back | Pass | Exact value returned before rollback restart |
| Normal stop and same-package ureq restart | Pass | NBReq final joins 3/1 ms; ureq selected at 15:21:02 |
| Ureq primary/backup and safe login/read proof | Pass | Both channels and authenticated website use passed; zero unexpected errors |

All 390 actual NBReq responses mapped to successful POSTs. The 36 fetched-only snapshot entries
correlated with continuing application long polls; no response existed without its fetch. No NBReq
error/timeout/limit or credential-header diagnostic appeared. G6 is accepted for the controlled
curl pilot, not as a broad release soak or as relaxation of the native backend destination.
