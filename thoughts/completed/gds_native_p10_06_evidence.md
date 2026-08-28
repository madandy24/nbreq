# GDS native NBReq P10-06 evidence

Status: **P10-06 accepted on Windows 10 and Ubuntu 20.04/stock Wine 5.** Windows used the original
authenticated package for the 81-minute persisted-setting canary. Wine exposed and then closed a
real platform compatibility hole; the repaired authenticated successor passed public process-local
selection, live native traffic, prompt cancellation/join, normal shutdown, and same-package ureq
rollback. Ordinary NBReq construction remains unchanged; this does not itself authorize P10-07.

## Accepted Windows package

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

That drill accepted the Windows 10 half of P10-06. It was a correctness and lifecycle canary, not a
fleet, performance, verified-TLS, or ordinary-constructor claim. The following section records the
subsequent stock-Wine-5 repair and closes the second platform half.

## Accepted Wine-repaired package

The first stock-Wine repetition used the Windows-accepted source and failed before HTTP
initialization with Mio's `Failed to open \\Device\\Afd\\Mio: Path not found`. A standalone
32-bit native probe reproduced the same failure outside GDS. Trying the base `\\Device\\Afd`
object also failed, matching the known absence of Mio's private AFD readiness route on old Wine
([upstream issue 1444](https://github.com/tokio-rs/mio/issues/1444)). This was an NBReq platform
compatibility defect, not a GDS ownership or HTTP-facade failure.

NBReq commit `6c3bde6d5feac7fce0beebeab77e9d4cd5a430a1` adds a narrow readiness abstraction. Native Windows
continues to use Mio. Only a first-registration `NotFound` error naming `\\Device\\Afd` switches
that poll owner to documented WinSock `WSAPoll`; the fallback never occurs after any socket has
registered. Because old Wine cannot use Mio's completion-port waker either, the compatibility wait
is capped at 50 ms so submit, cancel, and shutdown cannot strand. NBReq proper retains
`unsafe_code = "forbid"`; the small private, unpublished `nbreq-winpoll` workspace crate contains
the audited WinSock FFI and exposes only a safe readiness API.

The forced-fallback Windows test covers connect, write, read, FIN, and cancellation. The ordinary
Windows account passes the expanded 21-step verification entry point in 64.583 seconds, including
the combined Schannel suite. A rebuilt 32-bit standalone probe then returns HTTP 200 with a
559-byte body under stock Wine 5, exercising automatic fallback, DNS, TCP, HTTP, and joined exit.

The repaired GDS package is:

- Archive: `gds-nbreq-native-x86.zip`.
- Archive size: 15,948,147 bytes.
- Archive SHA-256: `BB492B60E100C89B40D0772311C5D7A47D7364F24D3D1BC5BE0D2DC466E37C37`.
- GDS source: `7d4d24325aa7684da75db13d3660567e84d95438`.
- NBReq source: `6c3bde6d5feac7fce0beebeab77e9d4cd5a430a1`.
- GDS DLL SHA-256: `3274EE3C96C4E713F9818FD75165C115CDD888A5C59C717F14CA45FDA2869B62`.
- Delphi host SHA-256: `0238DF8571FA32C63F3F63E3320937B0DC9F4A73B1FE37CABF559733555FA018`.
- PDF font data SHA-256: `7DCECDB17867500E590C2EAEB491E53E5D68CA24D0A41FBB90043307EE487CA0`.
- Wine-5 ProcessPrng shim SHA-256:
  `C199985B0035F332E71CDC597F2568E05FB24B558EC11EDED456732B731EDA0F`.

The archive hash matched after copy. All 11 extracted manifest entries passed `sha256sum -c` in a
fresh `/home/ubuntu/gds-nbreq-native-wine5-6c3bde6` evidence directory. The package verifier had
already proved the required x86 binaries, native marker and public selector exports, Delphi runtime
data, and absence of libcurl.

## Stock-Wine-5 drill record

The owner-selected consumer host is Ubuntu 20.04.6 LTS with distro Wine 5.0. It contains test data
only. The prior accepted package and its artifacts were left intact.

| Gate | Result | Evidence |
|---|---|---|
| Copied archive and extracted manifest | Pass | Exact 15,948,147-byte archive hash plus all 11 extracted hashes matched |
| Unknown public selector fails closed | Pass | `/httpbackend definitely-invalid` stopped startup before HTTP initialization and named the override source plus all three valid values |
| Public native process-local selection | Pass | Log records `nbreq-native`, says persisted `DSHTTPBACKEND` is ignored for that process, and names NBReq native plus the explicit GDS no-verify policy |
| Live native startup and traffic | Pass | Green board, both long-poll channels, authenticated website login/navigation, and ongoing Activity-screen traffic succeeded; a second fresh native start repeated green-board and website success |
| Native refresh during active polls | Pass | Both requests reported expected cancellation; shutdown completed in 1 ms and Drops joined in 7 ms and 2 ms |
| Normal native close | Pass | Both active polls cancelled; shutdown completed in 1 ms and Drops joined in 4 ms and 1 ms; exact-name post-close check found no `gds.exe` |
| Same-package public ureq rollback | Pass | `/httpbackend ureq` was logged from the same hashes; board and authenticated website were healthy |
| Normal rollback close | Pass | GDS Drop returned in 0-1 ms; ureq retained its known detached 1.077 s / 11.474 s worker completion; final exact-name check found no process |

The first repaired live run remained healthy for roughly 22 minutes before normal close. Its fresh
directory initially lacked `logs/`, so the exact selection and join markers were captured on the
immediate second native start after creating that standard directory. The first run still provides
independent owner-observed green-board, website, long-poll, and normal-close evidence; the second
run binds those observations to exact backend and lifecycle markers. The only package-local error
record is the deliberate invalid-selector test. No libcurl was present or loaded.

P10-06 is accepted. This remains a controlled correctness/lifecycle result, not a fleet rollout,
performance attribution, verified-TLS claim, or permission to remove ureq/curl rollback. P10-07 may
now review the separate ordinary-construction/default-feature switch; it must compile native for a
plain `cargo add nbreq`, keep curl explicitly selectable without feature-unification side effects,
and keep no-default-feature construction fail-closed rather than exposing the scaffold as a public
runtime.
