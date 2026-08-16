# WP2 Curl Pilot Evidence

Status: Windows x64 transport and process-lifetime DLL slice proven on 2026-08-16; WP2 remains in
progress for controlled connect/DNS proof, Windows 10, Ubuntu 20.04/Wine, and native Ubuntu 20.04.

## What now exists

- One private curl `Multi` and all easy handles live on the spawned Engine reactor thread. Curl
  handle types do not cross the backend boundary and manual curl remains unsupported.
- The command queue holds curl's thread-safe `MultiWaker`. Submit, cancel, cancel-all, and shutdown
  wake `curl_multi_poll` directly; periodic polling is not the correctness mechanism.
- Cancellation commits the canonical `Cancelled` result first, then removes the easy handle on its
  owner thread. Local server EOF proves network ownership was actually released.
- Curl header/write callbacks collect owned data only. User callbacks remain in the WP1 dispatcher
  and are joined independently of the reactor.
- The private `testing::curl_engine` constructor permits proof work without making backend choice or
  curl types part of the stable public API.

## Compatibility profile implemented

- HTTP/1.1 is forced and only `http`/`https` URLs are accepted.
- Environment proxies are disabled. Cookie storage/handling is compiled out of the pilot DLL.
- Automatic decompression is absent and the pilot DLL has no zlib, Brotli, or zstd dependency.
- Automatic `Expect: 100-continue` is suppressed unless the caller explicitly supplies `Expect`.
- HTTP 4xx/5xx remain completed responses.
- Redirects are handled by NBReq rather than curl defaults: 301/302 follow GET and HEAD but do not
  rewrite POST/other methods; 303 becomes GET except HEAD remains HEAD; 307/308 preserve the
  buffered method/body; HTTPS downgrade is rejected; origin-bound credentials are stripped across
  origins; total timeout spans redirect hops; and a bounded hop limit is enforced.
- TLS certificate-chain and hostname verification remain the default. The explicit
  `DangerouslyDisableCertificateVerification` compatibility setting disables both checks for the
  legacy GDS cases that still require it. Controlled certificate fixtures remain a WP4/GDS audit
  item.

## Pinned Windows package

The repeatable build is `tools/build-curl-windows.ps1`.

| Item | Recorded value |
|---|---|
| curl source | Official `curl-8.21.0.tar.xz` |
| Source SHA-256 | `AA1B66A70EACE83DC624508745646C08AE561DE512AB403ADFFB93AC87FC72E6` |
| TLS/root provider | Schannel and the Windows certificate stores |
| Protocols | HTTP, HTTPS |
| Resolver | curl threaded resolver (`AsynchDNS`) |
| Other runtime libraries | None shipped; only Windows system DLL imports |
| Rust binding | local pinned fork of `curl` 0.4.50, upstream revision `0cfd9e3b8b1aa0b8fc2c8d552597555a30a21416` |
| FFI crate | `curl-sys` 0.4.90+curl-8.21.0 |
| Test toolchain | rustc 1.97.1, MSVC 19.44.35222, Windows 10.0.26200.9168 |

The package builds the C runtime statically into libcurl. `dumpbin /dependents` records only
`bcrypt`, `advapi32`, `crypt32`, `secur32`, `ws2_32`, `iphlpapi`, and `kernel32`. No ambient OpenSSL
or compression DLL can be selected. `tools/test-curl-windows.ps1` creates a controlled vcpkg-rs
discovery tree, forces a fresh dynamic Rust link, checks `vendored=false`, version 8.21.0, Schannel,
and no libz, then verifies every curl-sys DLL copy against the selected artifact hash. The artifact
hash is emitted for every build because PE build metadata can make it build-specific; the source
hash and build recipe are the stable pin.

The curl source uses the curl license. The Rust transport/build dependency set currently reports
MIT, `MIT OR Apache-2.0`, or `MIT/Apache-2.0`; exact notices must be copied into the eventual pilot
package before GDS deployment.

## Loader and shutdown decision

Upstream Rust `curl` 0.4.50 invokes `curl_global_init()` from a Windows CRT constructor. That is not
acceptable in a DLL under loader lock. The pinned local fork adds only `nbreq-explicit-init`, which
disables the constructor. NBReq explicitly calls `curl::init()` when its spawned reactor constructs
the curl backend, on an ordinary thread after an exported API call.

The binding deliberately never calls `curl_global_cleanup()` because it cannot prove that all
process threads are safe for global cleanup. Consequently, **the curl-backed GDS pilot does not
support `FreeLibrary`-based unload**. The host must preload the pinned `libcurl.dll` by absolute
path, verify that path, load the GDS/probe module, and pin both modules until process exit. Engine
shutdown still cancels every request, destroys every easy/Multi handle, joins the reactor, drains
callbacks, and leaves no Engine-owned curl activity. This restriction belongs only to the curl
pilot, not to the native destination.

The private `experiments/windows-curl-dll` probe uses that controlled preload, performs an actual
callback HTTP request, drains and joins callback/network workers, and shuts down. Twenty-five fresh
host processes passed load/use/exit. Fresh-process repetition is the supported lifecycle; it is not
misrepresented as in-process unload/reload.

## Measured Windows results

The exact dynamic package ran concurrent GET, POST, HTTP 404, redirects, total timeout, peer wakeup,
individual cancellation, and Engine shutdown tests. On the recorded machine:

- command submission woke a reactor already blocked in curl Multi and completed its peer request;
- individual cancellation did not harm the peer transfer;
- 10 slow-header cancellation trials had a maximum observed socket-release latency of 3.8892 ms;
- 10 stalled-body cancellation trials had a maximum observed socket-release latency of 3.0735 ms;
- the provisional supported-platform gate is **less than 100 ms** from cancellation request to the
  controlled peer observing socket closure.

The 100 ms value is provisional until the same named-stage tests run on every supported target.
Waiter notification is earlier than socket release because WP1 commits terminal state synchronously;
the measurement above intentionally covers backend teardown as well.

## Remaining WP2 gates

- Add a deterministic connect-stage fixture and measure actual backend removal, not only canonical
  waiter cancellation.
- Exercise controlled DNS/resolver cancellation and prove shutdown leaves no resolver thread for
  the exact package.
- Run the same package/tests on the minimum Windows 10 x64 target and the Windows artifact under
  Ubuntu 20.04's default Wine.
- Build/inventory the native Ubuntu 20.04 shared-libcurl package and run the same contract suite.
- Apply the absolute-path preload/pinning rule to the exact GDS artifact during WP5 packaging.
- Add controlled TLS certificate fixtures proving verified-default and explicit no-verify behavior.
- Produce the final dependency notices and pilot security-update checklist.

Security updates retain the version/hash constants, review curl and Rust-binding advisories plus
release notes, rebuild in a clean target directory, rerun capability/dependency/hash checks, rerun
the lifecycle and latency suites on every supported platform, and replace the adjacent DLL only as
part of a versioned pilot package with the ureq rollback still available.
