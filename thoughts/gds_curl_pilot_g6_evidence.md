# GDS curl-pilot G6 evidence

Status: authenticated DMOUSE2 candidate package ready; live ureq-baseline, persisted activation,
60-minute health observation, and ureq rollback remain pending.

## Candidate package

- Target: DMOUSE2, Windows 10 Pro 22H2, GDS `#C`.
- Archive: `gds-nbreq-curl-pilot-g6-0a210ab-a70c63f-x86.zip`.
- Archive SHA-256: `7F99731833AB8F3EE9817C0CBA5325ECF6AB1F05618B5C9546DF8BC0486DA774`.
- Archive size: 15,919,242 bytes.
- GDS source: clean detached commit `0a210abae00649d72fdcdf50f2f5ee74dba8af3c`.
- NBReq source: clean detached commit `a70c63f9d345da146a454398d0741c6841faf2e3`.
- Delphi host SHA-256: `A262318853EFB316D5359E6833AC12115010183EAC9E9A77F98A844DE277746E`.
- GDS DLL SHA-256: `730A22A59782625FBF47713FCE5C656F6BB2A5FBD5303F0EF439FCC6FE50A0E3`.
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
`C:\User\projects\nbreq\target\curl-pilot\gds-nbreq-curl-pilot-g6-0a210ab-a70c63f-x86.zip`.
The Windows payload is the complete `windows-10-x86` directory; do not mix it with the Wine
variant. The archive also carries the verifier, notices, build provenance, and G6 runbook.

The same archive is staged and extracted on DMOUSE2 at
`C:\adstemp\nbreq-g6-0a210ab-a70c63f`; the verifier passed against those remote files before the
live drill. The earlier superseded staging directory was removed to prevent operator ambiguity.

## DMOUSE2 drill record

Follow `gds_curl_pilot_g6_runbook.md`. Record evidence here as the drill proceeds; none of the
unchecked rows is implied by package construction.

| Gate | Result | Evidence |
|---|---|---|
| Extracted package verifier on DMOUSE2 | Pass | 15 hashes, x86 payload/runtime data, and selector export verified over the staged share |
| Live files match the four Windows hashes above | Pending | |
| Same-package ureq baseline and both channels healthy | Pending | |
| Persisted `system_DSHTTPBACKEND=nbreq-curl-pilot` read back | Pending | |
| Normal stop and fresh start without private selector | Pending | |
| Exact NBReq startup policy/path markers | Pending | |
| Primary and backup complete at least two polls each | Pending | |
| Safe authenticated login/read plus `OK` response POST | Pending | |
| Settings refresh cancels/joins/recreates both pollers | Pending | |
| 60-minute health interval | Pending | |
| Persisted `system_DSHTTPBACKEND=ureq` read back | Pending | |
| Normal stop and same-package ureq restart | Pending | |
| Ureq primary/backup and safe login/read proof | Pending | |

G6 remains open until every row is resolved with log times and the operator accepts the health and
rollback thresholds. This package is a canary candidate, not a wider-release authorization.
