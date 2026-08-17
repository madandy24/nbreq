# GDS curl-pilot G4 packaging and loader evidence

Status: G4 accepted on 2026-08-17. The final binary set passed an exact-host proof on native
Windows, and the final authenticated package passed on Ubuntu 20.04.6 LTS with stock Wine 5.0. This
is packaging/lifecycle evidence, not a live NBReq HTTP canary or authorization to select NBReq in
production.

## Frozen source and artifact

- GDS commit: `51269a088e06933e885911f1aa742b012b3ad1bc`
- NBReq commit: `81d5fd9f05ae481a05fa6905963e73ecd7d10046`
- both repositories recorded `clean: True` before the evidence build;
- archive: `target/curl-pilot/gds-nbreq-curl-pilot-g4-51269a0.zip`;
- archive size: 15,906,339 bytes;
- archive SHA-256: `963F8891D9156D86CFD7A10FBE1BC29087F7DCB555114FB22A0C376CA2BFAC77`;
- Delphi GDS host SHA-256:
  `EAE99204FABAC0123E7ACE00F24B42FC25F0D8720C770DAEE5394FAE0F35E4A1`;
- GDS DLL SHA-256: `593DDFC50D3EFB9BE3E9CDFF5D82C34EEC7C8F6F2E6A49D3800F703F62A1ACE8`;
- libcurl 8.21.0 SHA-256: `C9DF3A41B6CBD3230B9BAD63E4FCEAE31667CBA15C9033B544E1500BCD2E0530`;
- packaged `PDFFontData.dat` SHA-256:
  `DE9AA30FD9AF5ECDEDDEE79E4278B092DBFAF00E535AE69BA920F92C3E1B148E`;
- clean-build Wine-5 shim SHA-256:
  `E021C0DDCA643F6EF9F1FCE686D789D7DE774B844AB6EFB44A461F27D592F344`.

The archive contains `windows-10-x86` (`gds.exe`, `gds.dll`, `libcurl.dll`, `PDFFontData.dat`) and
`wine5-x86` (the same four files plus `bcryptprimitives.dll`). It also contains `BUILD-INFO.txt`, a
complete SHA-256 manifest, a Windows verification script, the curl license, pilot notices, and
handling instructions. The verifier checks all fourteen manifest entries, all seven executable/DLL
copies are x86 PE, and both platform folders contain the Delphi host's required adjacent font data.
The portable ZIP uses forward-slash entries and its manifest is LF-only, so ordinary Linux `unzip`
and `sha256sum -c manifest.sha256` work without normalization.

An initial target-host extraction rehearsal caught PowerShell `Compress-Archive` backslash entries
and a CRLF manifest before any host launch. GDS commit `7a1a7e7` replaced that archive path with a
portable ZIP writer, emitted an LF manifest, and added the exact clean-build Delphi host. That
initial archive is superseded and is not evidence.

The first exact-Wine host launch exposed a separate GDS packaging assumption: moving `gds.exe` away
from an installed client directory without its adjacent `PDFFontData.dat` caused a Delphi startup
exception before HTTP selection. Copying the installed comms-role asset beside the proof host made
that package run normally and proved the diagnosis. GDS commit `51269a0` then made the asset a
required, hashed member of both package variants and of the verifier. The final clean Git form is
LF-only rather than the installed copy's CRLF form; the final exact-host run proves GDS accepts it.

The Wine shim is built from NBReq's one-export source and delegates `ProcessPrng` to
`bcrypt.dll!BCryptGenRandom`; it contains no copied Windows/Wine implementation and is absent from
the native-Windows package.

## Loader and pinning proof

`TdsRustInterface.LoadDLL` now:

1. resolves `/rustdll` (or the default `gds.dll`) to an absolute path;
2. on native Windows, loads with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR |
   LOAD_LIBRARY_SEARCH_SYSTEM32`;
3. on Wine, loads the absolute path with `LOAD_WITH_ALTERED_SEARCH_PATH`, allowing the exact
   adjacent dependency set on the legacy loader;
4. verifies the loaded GDS path with `GetModuleFileName`;
5. reads the optional `nbreq_curl_pilot_compiled` export (older DLLs remain compatible and ordinary
   ureq builds return zero);
6. for pilot builds only, requires `libcurl.dll` beside the selected GDS DLL, verifies that the
   module already resolved by the Rust import is that exact file, and takes a separate retained
   module reference; and
7. retains both handles in the process-owned Delphi data module. There is no `FreeLibrary` call.

The marker test passed in both configurations: ordinary/no-feature returned zero and
`nbreq-curl-pilot` returned one. The Delphi project compiled successfully with 578 existing warnings
and 1,746 existing hints; none came from the modified loader file. The clean x86 pilot release built
successfully with its existing warning baseline and exported the marker.

On the local native host (`Microsoft Windows NT 10.0.26200.0`), the exact clean packaged Delphi
`gds.exe` was started as the `#C` client with `/rustdll` naming its adjacent packaged DLL. At
2026-08-17 19:48:27 the GDS log recorded:

- the requested clean-package `windows-10-x86/gds.dll` absolute path;
- the loaded Rust DLL at that exact path; and
- the pinned `windows-10-x86/libcurl.dll` at its exact adjacent path with `Wine=False`.

The process remained responsive and was then terminated by its exact proof PID. Ureq remained the
runtime HTTP selection throughout; this proved loader/package behavior only.

## Stock-Wine exact-host proof

Bridge preflight confirmed the owner-selected target is Ubuntu 20.04.6 LTS with stock Wine 5.0
(`Ubuntu 5.0-3ubuntu1`). The final archive was copied unchanged to the host, its archive SHA-256
matched, and all fourteen manifest entries passed `sha256sum -c` after extraction into the fresh
`/home/ubuntu/nbreq-g4-51269a0` directory.

The exact packaged `wine5-x86/gds.exe` was launched as the test `#C` role with the configured
`/rustdll` naming `Z:\home\ubuntu\nbreq-g4-51269a0\wine5-x86\gds.dll`. At
2026-08-17 20:07:43 the GDS log recorded:

- the requested and loaded Rust DLL at that exact absolute path;
- the pinned adjacent `libcurl.dll` at its exact absolute path with `Wine=True`;
- compatible Rust exports, callback initialization, embedded data-dictionary loading, and complete
  Rust initialization; and
- `use_rust_api=False`, confirming ureq remained the runtime HTTP selection.

The owner observed the client running normally. Existing GDS test-role background traffic received
HTTP 200 responses through the still-selected ureq path; no NBReq live-backend claim is inferred.
Wine emitted its existing missing-ODBC-driver and old-NTLM-helper diagnostics, neither of which
prevented startup or operation. After the owner terminated the proof instance, an exact-name process
check reported no remaining `gds.exe`. No `FreeLibrary` unload/reload was attempted, and the shim was
not installed into Wine or exposed through ambient PATH.

G4 is therefore accepted. Windows-10 target behavior, the selected NBReq adapter, controlled
gateway/long-poll behavior, public selection, and rollback remain G5-G6 work.
