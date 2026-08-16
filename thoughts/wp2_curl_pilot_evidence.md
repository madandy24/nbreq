# WP2 Curl Pilot Evidence

Status: Windows 10 x64 transport, TLS certificate-policy, and process-lifetime DLL slices, native
Ubuntu 20.04 transport/TLS compatibility, and stock-Wine-5 transport/no-verify compatibility proven
by 2026-08-17. The stepping-stone explicitly does not claim prompt connect/DNS network teardown;
WP2 remains in progress for packaging/notices, exact GDS integration, and Wine's verified-trust
limitation.

## What now exists

- One private curl `Multi` and all easy handles live on the spawned Engine reactor thread. Curl
  handle types do not cross the backend boundary and manual curl remains unsupported.
- The command queue holds curl's thread-safe `MultiWaker`. Submit, cancel, cancel-all, and shutdown
  wake `curl_multi_poll` directly. The hardening seam additionally latches wake failure and uses a
  short bounded safety poll so a failed wake cannot turn the old 24-hour proving deadline into an
  indefinite stall; periodic polling is not the normal latency mechanism.
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
  legacy GDS cases that still require it. A generated local TLS fixture now proves trusted success,
  wrong-host rejection, unknown-root rejection, expired-certificate rejection, and explicit
  no-verify success without changing an OS trust store or checking in private material. The exact
  GDS configuration mapping remains a WP4/WP5 audit item.

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
package before GDS deployment. `rcgen`, `rustls`, and their locked dependencies are test-only TLS
fixture tooling; they do not enter NBReq's runtime dependency graph or the GDS curl pilot package.

## Loader and shutdown decision

Upstream Rust `curl` 0.4.50 invokes `curl_global_init()` from a Windows CRT constructor. That is not
acceptable in a DLL under loader lock. The pinned local fork adds `nbreq-explicit-init`, which
disables the constructor, plus a fallible `curl::try_init()` extension. NBReq calls that extension
when its spawned reactor constructs the curl backend, on an ordinary thread after an exported API
call. The first initialization result is retained without a poisonable `Once`, so failure becomes
an Engine error rather than a repeated reactor panic.

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
individual cancellation, TLS certificate-policy, and Engine shutdown tests. On the recorded
machine:

- Schannel verification must run under a normal Windows process token. At the 56-test checkpoint,
  the exact same compiled vendored test binary passed under the normal account but a restricted
  Codex execution token deterministically failed all three Schannel fixtures; the stalled-handshake
  barrier observed its connection close before ClientHello. Re-running outside that restricted
  context passed the full matrix; the latest normal-account cleanup run passes 58 unit tests, 4
  public-contract tests, and 2 doctests. The restricted result is an execution-environment
  limitation rather than product evidence; the precise denied Windows
  facility has not been isolated or claimed.

- command submission woke a reactor already blocked in curl Multi and completed its peer request;
- individual cancellation did not harm the peer transfer;
- 10 slow-header cancellation trials had a latest maximum observed socket-release latency of 2.6216 ms;
- 10 stalled-body cancellation trials had a latest maximum observed socket-release latency of 5.8288 ms;
- the provisional supported-platform gate is **less than 100 ms** from cancellation request to the
  controlled peer observing socket closure.
- the generated direct trust anchor succeeded through Schannel; wrong host, unknown root, and
  expiry all produced portable `TransportStage::Tls` failures; the explicit no-verify request
  succeeded. This direct-anchor fixture proves policy without modifying the machine trust store;
- 10 deliberately stalled TLS-handshake cancellation trials had a latest maximum observed
  socket-release latency of 1.6263 ms, satisfying the same provisional 100 ms gate. The barrier
  observes ClientHello bytes before cancellation, so this is a TLS-stage rather than merely an
  accepted TCP socket.

The 100 ms value is provisional until the same named-stage tests run on every supported target.
Waiter notification is earlier than socket release because WP1 commits terminal state synchronously;
the measurement above intentionally covers backend teardown as well.

### Windows 10 minimum-target proof

The portable bundle produced by `tools/package-win10-proof.ps1` from commit
`f14bfc872d6d22007a5f51324a6640ccb2144276` ran under an ordinary user on Windows 10 Pro 22H2 x64,
build `19045.7663`, using Windows PowerShell 5.1. Every transferred bundle hash matched. Runtime
capabilities reported the pinned dynamic libcurl 8.21.0 with Schannel, asynchronous DNS, IPv6,
no zlib, and `vendored=false`.

- 58 unit tests and 4 public-contract tests passed;
- ten slow-header trials observed a 1.8148 ms maximum socket-release latency;
- ten stalled-body trials observed a 1.8458 ms maximum;
- ten TLS-handshake trials observed a 2.0508 ms maximum after the ClientHello barrier;
- 25 fresh-process absolute-preload/load/use/exit DLL probe iterations passed.

The returned transcript has SHA-256
`7FA7E080728323AF861B006581E50C2E81DD3CFDDB38E6E6A85C2B099E896F37`. This closes the native
Windows 10 platform gate for the curl pilot; exact GDS packaging and trust configuration remain
separate WP5 gates.

## Measured Ubuntu 20.04 native results

The native minimum-target run used an updated Ubuntu 20.04 installation, x86-64 kernel
`5.4.0-216-generic`, the declared Rust 1.85.0 MSRV, GCC 9.4, and Ubuntu's dynamically linked
libcurl 7.68.0 with OpenSSL 1.1.1f. Curl reports asynchronous DNS and IPv6; the distribution build
configuration confirms `--enable-threaded-resolver` and no c-ares dependency.

- The dependency-free/default matrix passed 31 unit tests, 4 public-contract tests, and 2 compile-fail
  doctests.
- The system-curl matrix passed 44 unit tests, 4 public-contract tests, and 2 compile-fail doctests;
  Rust 1.85 `cargo fmt --check` and clippy with warnings denied also pass.
- The 13 named curl transport tests pass against the non-vendored distribution library. Ten
  slow-header and stalled-body trials observed maxima below 0.1 ms on this host; ten TLS-handshake
  trials observed a 1.173882 ms maximum. All remain below the provisional 100 ms gate.
- The generated certificate fixture proves trusted success, wrong-host rejection, unknown-root
  rejection, expiry rejection, and explicit no-verify success through OpenSSL without modifying the
  machine trust store.

The minimum-target run found and closed two compatibility defects. The curl shutdown loop used an
`if let` chain newer than the declared Rust 1.85 MSRV; equivalent nested control flow now compiles
on 1.85. The test-only in-memory CA option (`CURLOPT_CAINFO_BLOB`) was added in libcurl 7.77, so the
fixture now uses a uniquely owned temporary CA file on older curl while retaining the blob path on
newer curl. This is test infrastructure only and does not alter production trust or no-verify policy.

The controlled DNS teardown probe deliberately holds one `getaddrinfo` call for 1.5 seconds. A
cancelled request becomes canonically terminal immediately, Engine shutdown waits for the resolver
to finish, and the process returns from 2 threads to its 2-thread baseline; there is no abandoned
resolver thread. However, cancel-to-network-shutdown measured **1.703454775 seconds**. The stock
threaded-resolver package is therefore joined and lifecycle-safe, but it cannot satisfy NBReq's
prompt DNS cancellation goal. This is an explicit curl-pilot limitation, not a relaxation of the
native destination: an exact pilot package needs a cancellable resolver such as c-ares before it may
claim the 100 ms DNS gate. The opt-in reproducer is under `experiments/linux-curl-resolver`.

## Measured Ubuntu 20.04 Wine 5 results

The exact Rust-1.85 Windows test executable and pinned curl 8.21.0 Schannel DLL were run with
`wine64 5.0` on the same Ubuntu 20.04 host. The executable initially failed in the loader because it
imports `bcryptprimitives.dll!ProcessPrng`; the adjacent libcurl DLL does not import that library and
depends only on normal Windows bcrypt/CryptoAPI/Schannel/Winsock/kernel APIs. Windows has supplied
`ProcessPrng` since Windows 8, but Wine 5 predates Wine's implementation.

`experiments/wine5-bcryptprimitives` therefore provides a test/deployment compatibility shim with
one export, `ProcessPrng`, implemented by calling Wine 5's existing
`bcrypt.dll!BCryptGenRandom`. The final MSVC build has no DLL entry point or C runtime dependency;
its only imported DLL is `bcrypt.dll`. It is independent of curl and is needed only when a produced
Windows Rust artifact itself imports `ProcessPrng`. The installed Delphi `gds.exe` inspected on this
host does not. The final shim is 3,584 bytes with SHA-256
`F7E01246997953A71E2EE819ED90AD044AEA0CAD4BA0D8677AB57A773EFE18B3`.

With the shim adjacent, 13 of 14 curl-focused tests pass under stock Wine 5. Concurrent HTTP,
redirects, limits, wakeup, shutdown, header/body cancellation, TLS-handshake cancellation, and the
explicit chain-and-hostname no-verify path all pass. Ten receive-stage trials remained below 0.4 ms;
ten TLS-handshake trials observed a 1.5505 ms maximum, well within the provisional 100 ms gate. The
single failure is verified use of the generated local trust anchor: Wine 5 Schannel reports a TLS
certificate rejection where native Windows Schannel and Ubuntu OpenSSL accept the same generated
trust policy. This is an explicit legacy-Wine trust limitation and supports retaining the existing
prominently named no-verify compatibility setting; it does not change NBReq's verified-by-default
contract. WP4/WP5 must still audit the real GDS endpoint and setting scope.

## Hardening seam completed

The post-slice review was applied before consumer API work:

- Configurable defaults now bound buffered request/response bodies at 16 MiB, request/response
  header storage at 64 KiB, and header fields at 256. Limits are checked before buffer extension and
  return backend-neutral `LimitKind` detail.
- The spawned reactor and backend factory are unwind-contained. A panic drops backend objects on
  the reactor thread, fails every accepted request with the canonical internal failure, wakes
  waiters/callbacks, and remains observable at Engine shutdown.
- `MultiWaker` failure is retained as a fatal Engine error. The curl backend advertises a 50 ms
  safety wait, so the reactor discovers a failed wake and checks inactivity without imposing that
  fixed cadence on the native destination or making periodic polling the ordinary latency path.
- Inactivity is measured by NBReq's monotonic useful-I/O progress clock rather than curl's
  whole-second average-low-speed option. A 100 ms stalled-body case passes against both the ordinary
  and exact pinned dynamic builds.
- Errors carry portable timeout, transport-stage, and resource-limit detail without exposing curl
  codes. The finer DNS/connect/TLS fixture campaign remains below.
- Body-bearing custom methods suppress curl's invented form content type unless the caller supplied
  one. The private feature is named `curl-pilot`, avoiding any suggestion that it is a downstream
  production backend selection.
- The pinned binding exposes fallible once-recorded global initialization; init failure is an
  Engine error rather than a poisonable process-wide panic path.
- Each informational or final response head receives the configured header byte/count budget
  independently; an intermediate `100 Continue` cannot consume the final response's allowance.

## Remaining WP2 gates

- Carry the curl-only connect/DNS limitation into pilot release notes and retain finite connect and
  total deadlines. The pilot does not claim the native backend's prompt stage-by-stage cancellation
  gate: building firewall/backlog fixtures or a separate curl resolver solely for the stepping-stone
  is intentionally deferred. The native backend must still prove deterministic connect cancellation
  and cancellable/bounded resolver teardown.
- Decide whether the exact GDS canary accepts the recorded Wine-5 verified-custom-trust limitation
  or needs a newer Wine/trust path. Windows 10, stock Wine 5 transport/cancellation/no-verify, and
  native Ubuntu 20.04 are proven separately.
- Apply the absolute-path preload/pinning rule to the exact GDS artifact during WP5 packaging.
- Produce the final dependency notices and pilot security-update checklist.

Security updates retain the version/hash constants, review curl and Rust-binding advisories plus
release notes, rebuild in a clean target directory, rerun capability/dependency/hash checks, rerun
the lifecycle and latency suites on every supported platform, and replace the adjacent DLL only as
part of a versioned pilot package with the ureq rollback still available.
