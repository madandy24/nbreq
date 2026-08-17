# GDS curl-pilot G4 packaging and loader evidence

Status: G4 implementation and local exact-host proof complete on 2026-08-17. The exact stock-Wine-5
host run remains required before G4 is accepted. This is packaging/lifecycle evidence, not a live
HTTP canary or authorization to select NBReq in production.

## Frozen source and artifact

- GDS commit: `96cf352a69050349b3244d2a150c41468a3eb4c6`
- NBReq commit: `ee04fb9539a493b837790c3d7f47e506fb4c72d1`
- both repositories recorded `clean: True` before the evidence build;
- archive: `target/curl-pilot/gds-nbreq-curl-pilot-g4-96cf352.zip`;
- archive size: 6,310,192 bytes;
- archive SHA-256: `4BD111342FCFFF4FF2BD63549F8D910A5F461268490F70F603B56A60C5A70047`;
- GDS DLL SHA-256: `593DDFC50D3EFB9BE3E9CDFF5D82C34EEC7C8F6F2E6A49D3800F703F62A1ACE8`;
- libcurl 8.21.0 SHA-256: `C9DF3A41B6CBD3230B9BAD63E4FCEAE31667CBA15C9033B544E1500BCD2E0530`;
- clean-build Wine-5 shim SHA-256:
  `E021C0DDCA643F6EF9F1FCE686D789D7DE774B844AB6EFB44A461F27D592F344`.

The archive contains `windows-10-x86` (`gds.dll`, `libcurl.dll`) and `wine5-x86` (the same pair plus
`bcryptprimitives.dll`). It also contains `BUILD-INFO.txt`, a complete SHA-256 manifest, a Windows
verification script, the curl license, pilot notices, and handling instructions. The verifier checks
all ten manifest entries and the PE machine field of all five DLL copies.

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

On the local native host (`Microsoft Windows NT 10.0.26200.0`), the real Delphi `gds.exe` was started
with `/rustdll` naming the clean packaged DLL. At 2026-08-17 18:01:01 the GDS log recorded:

- the requested clean-package `windows-10-x86/gds.dll` absolute path;
- the loaded Rust DLL at that exact path; and
- the pinned `windows-10-x86/libcurl.dll` at its exact adjacent path with `Wine=False`.

The process remained responsive and was then terminated by its exact proof PID. Ureq remained the
runtime HTTP selection throughout; this proved loader/package behavior only.

## Remaining G4 target proof

Bridge preflight confirmed the owner-selected target is Ubuntu 20.04.6 LTS with stock
Wine 5.0 (`Ubuntu 5.0-3ubuntu1`). No application configuration or customer data was inspected.

Copy the authenticated archive unchanged to the Ubuntu 20.04 host, verify its archive hash, extract
into a new isolated directory, and run `sha256sum -c manifest.sha256` from the package root. Start
the existing Delphi GDS host under stock Wine 5 with `/rustdll` naming the absolute Wine path to
`wine5-x86/gds.dll`. Record:

- Wine and Ubuntu versions;
- manifest success;
- exact Rust and curl module-path log lines with `Wine=True`;
- successful host initialization; and
- process exit without attempting `FreeLibrary` unload/reload.

Do not copy the shim into Wine system directories, alter ambient PATH, select NBReq as the live HTTP
backend, or contact a production gateway for this G4 proof. Windows-10 and live gateway/long-poll
execution remain G5 work.
