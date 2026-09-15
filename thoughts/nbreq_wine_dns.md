# W-01 — Windows DNS discovery under Wine

2026-09-10. **Reproduced and fixed at the NBReq Windows support boundary.** Actual Wine 5
execution and native Windows checks pass. GDS's deployed DLL identity and end-to-end
startup/remote-scan/shutdown acceptance remain for the GDS session. Nothing was published.

**2026-09-14 pilot update:** The owner reports a live GDS pilot under Wine has run this code
for several days with no problems observed. This adds real application evidence to the focused
reproduction/fix checks. Exact pilot DLL hash, Wine/prefix identity, workload and an explicit
shutdown/remote-scan acceptance transcript were not supplied; do not infer those details.
W-01 does not block continuing the library release gates. The technical record below preserves
what this task directly verified on 2026-09-10.

## Resume checkpoint

| Item | State |
| --- | --- |
| Source | `codex/wine-dns-adapters`, worktree `target/worktrees/nbreq-wine-dns`, based on clean root checkpoint `7693d6cf6f702432235df4001b8ef1731d70a25e`. Main's unrelated work and both earlier candidate branches are preserved. |
| Root cause | Wine 5 returns odd, in-buffer, terminated Description/FriendlyName pointers. Unchanged ipconfig 0.3.4 decodes them through widestring 1.2.1 and reproduces the exact `ucstr.rs:939` misaligned-u16 non-unwinding panic. |
| Fix | nbreq-winpoll **0.1.1** reads only DNS-relevant fields through checked byte slices. NBReq remains **0.2.0** and removes ipconfig/widestring; its public API and memory limits do not change. |
| Verification | Windows full verifier 24/24; helper 13/13 on Windows x86, Windows x64/Rust 1.85 and actual 32-bit Wine 5. Root policy/discovery 8/8 on x86 and x64/MSRV, plus 9/9 native-only x86. Ordinary default/native-only DNS/HTTP/shutdown probes pass on native Windows and actual Wine. Failed attempts remain documented below. |
| Evidence lab | Main workspace `target/wine-dns`; retained diagnostic source/lockfile in `tools/wine-dns-probe`. Final source/package and evidence identities are recorded in the accompanying artifact manifest. |
| Next | GDS rebuild and startup/scan/shutdown acceptance. Release R3 now also needs winpoll 0.1.1 publication; final soak/CI and registry-only acceptance remain open. |

## Reproduction and diagnosis

The user-confirmed Linux bridge host is x86_64, kernel 5.4.0-216-generic, with **Wine 5.0
(Ubuntu 5.0-3ubuntu1)** and wine32:i386/libwine:i386. Tests used a new private prefix
`/tmp/nbreq-wine-dns-lab-20260910/prefix`, `WINEARCH=win32`, reporting Windows 7.
This reproduces the library fault on the indicated host independently of GDS; it is not a run
inside the original application's unidentified prefix.

The original Rust 1.97.1 MSVC debug x86 probe is 228,352 bytes, SHA256
`510d219331fb54b86dbac280945e23c23b6b912f2c7d61b2c67faa69e0133e95`.
It inspects the owned GetAdaptersAddresses buffer before converting strings, then calls unchanged
ipconfig in a separate `--legacy` process. Inspection reads UTF-16 as byte pairs and uses a
bounded `read_unaligned` for diagnostic structure inspection only.

- The x86 structure has size 376, alignment 8, Description offset 36 and FriendlyName offset 40,
  matching official bindings and ipconfig's generated layout assertions.
- Two of Wine's three adapters have odd Description/FriendlyName pointers, in bounds and
  terminated. Later adapter records are themselves unaligned, at offsets 719 and 1614.
  This rules out the suspected layout mismatch for this reproduction.
- The legacy call aborts with the exact widestring alignment panic at `ucstr.rs:939`, exit 5.
  No GDS loader, database or other DLL code is involved.
- Native x86 Windows returns eight adapters with even descriptive pointers and no legacy panic.
  Some records have only four-byte alignment; the new byte parser handles them too.

The incompatible alignment assumption is confirmed. The original deployed DLL hash, application
prefix and why earlier Wine observations missed it remain unknown. Imprecisely symbolized names
such as `dpgpi_send_data` are not used for attribution. A release-mode run without an alignment
panic would not establish that the old reads were valid.

## Fix and preserved behavior

The existing unsafe support boundary calls GetAdaptersAddresses, skipping unused address lists
and the friendly name. Merely asking the OS to omit metadata is not the safety mechanism:
Description, FriendlyName and DnsSuffix pointers are never followed at all. They may be null,
odd, outside the buffer, unterminated or invalid UTF-16 without affecting DNS.

The OS receives initialized, aligned owned storage. Subsequent parsing uses checked byte slices
and SDK field offsets; no returned structure, socket or string pointer is dereferenced. Required
record lengths, pointer ranges, signed socket lengths, family sizes, identifier termination/
encoding and list cycles are checked. Resizing has five attempts and fallible allocation.
No new fixed cap rejects otherwise valid large adapter configurations.

NBReq retains up-interface filtering, family-specific metric ranking, stable adapter/server
ties, port 53, deduplication and unspecified-address rejection. Registry search-list precedence,
ranked adapter suffixes, domain fallback and normalization remain unchanged; native-only still
omits public search suffixes. Explicit IPv6 scope IDs are retained, with the existing interface-
index fallback for link-local addresses whose scope is zero. ipconfig discarded explicit scopes.

The production lock removes ipconfig 0.3.4 and widestring 1.2.1 and changes only nbreq-winpoll
0.1.0 to 0.1.1. Its existing windows-sys 0.61.2 dependency gains IP Helper/Foundation/NDIS bindings.
Socket2 remains dev-only. The unpublished diagnostic tool has its own committed lock and retains
the legacy dependencies solely for reproduction; it is excluded from the published root package.

There is no catch_unwind, alignment suppression, Cargo registry-cache patch, public DNS fallback,
GDS loader change or database change. Polling implementation, public APIs and memory knobs are unchanged.

## Verification and limits

| Check | Result |
| --- | --- |
| Support regressions | 12 DNS tests plus the polling test pass on native Windows x86 stable, Windows x64 Rust 1.85 and actual Wine 5 x86. Odd records/nodes/sockets and unused UTF-16, malformed required inputs, cycles, OS errors and bounded growth are covered. |
| Root discovery/selection | 8 tests pass on Windows x86 stable and x64 Rust 1.85; 9 pass in x86 native-only configuration, including live system discovery and resolver ownership/join. |
| Ordinary Windows probes | Default/native-only pass system discovery, exact loopback HTTP body, hostname HTTP (200) and joined shutdown. Default also passes public Resolver lookup. |
| Actual Wine probes | Both configurations pass the same checks; final rebuilt helper passes 13/13. Debug builds with alignment checks enabled, no nameserver overrides. |
| Full Windows verifier | Final unchanged-source run passes all 24 steps in 81.561 seconds, including formatting, warning-denied lints, feature/doc/example checks and pressure regressions. This is native Windows, not the entire suite under Wine. |
| Advisory/license checks | No unignored vulnerabilities or warnings against RustSec commit `b50980aad8b8f14f77e25a97b32dd94bf008b0af` (2026-09-09). Existing dev-only time exception RUSTSEC-2026-0009 remains; scan uses `--no-fetch --no-yanked`. Frozen all-feature license generation removes runtime ipconfig/widestring/socket2 entries and updates winpoll. |

The first verifier attempt found a Clippy test-module placement issue, corrected without runtime
changes. A later run overlapped builds/license work and reported five DNS/TCP failures before
hanging in `native_http_stalled_response_classifies_inactivity_and_total_timeouts`. That fixture
uses a 40 ms total request deadline and unbounded server accept/join; its factory has no OS
resolver, so it does not execute the new discovery path. The exact stuck test process was
identified and terminated; its suite returned failure.

All six named tests then passed individually on the same binary, and the unchanged full verifier
passed with competing builds stopped. Load sensitivity is plausible, **not proven**: the five
original panic details were buffered by the unfinished test harness and were not recovered.
No timeout, assertion or test success condition was relaxed. Preserve `root-verifier*.log`,
`focused-failures.json` and all six focused logs. The unbounded fixture cleanup merits separate
work; reruns do not dispose of every possible intermittent DNS/TCP defect.

Earlier cross-platform release/memory checks remain historical evidence. This change did not
rerun the whole Linux/macOS matrix or GDS workload. Synthetic scope/ranking tests do not imply
the Wine host had a live scoped IPv6 DNS configuration.

## Wine runtime and artifacts

Modern Rust initially stopped before main because Wine 5 lacks bcryptprimitives.dll!ProcessPrng.
The existing `experiments/wine5-bcryptprimitives` source was rebuilt unchanged with MSVC as Win32,
SHA256 `92d437dc538ef6ddfae7fc0b2140ec407a6dacfb0c899ec67a6812e84387de86`, 3,072 bytes.
It delegates to Wine's BCryptGenRandom and changes neither DNS parsing nor panic handling.
It was placed beside probes only, never installed in prefix/system directories. GDS packaging
should retain its separately documented legacy-Wine runtime handling.

Final inputs, also retained in `inputs.json`:

| Binary | SHA256 |
| --- | --- |
| Default probe | `611c67eb7da92cc96b1bffdb9e026fc3a591dbf2a1f5bea6ef60f21be7ed4750` |
| Native-only probe | `5fef5ceaf8c4e1c4ff2636c50a8407e2e5112ff2ce295a75420001109c507f68` |
| Helper tests | `24a896e56a1a1da1662376da268841f482982d42a7cd383a09f5860b4623e83b` |

Final remote lab: `/tmp/nbreq-wine-dns-gate-final-20260910`. Gate request
`20260910-051015-d845ed2c` exits 0; collected evidence archive SHA256
`e7fa6943fdf3b4fdf0e17fbaa49fc6b4f636b87a23ff717bb8d38e155b852b3b` was checked locally.
All jobs finished; only the private prefix's wineserver was stopped.

Two setup mistakes remain recorded: an older shim was uploaded with a stale hash in its reason
but never executed, then replaced by the fresh build; a nested-quote bridge log query left grep
waiting on stdin and only its matching Plink process was stopped. Later commands use uploaded
bounded Python runners and simple paths. Initial local audit/test redirection permission errors
ran neither command; both completed after permitted execution with the same lab paths.

## GDS handoff

No NBReq public API migration is needed. GDS already requires 0.2.0. Its existing
`gds/rust/gds/build.ps1 -LocalNbreq C:/User/projects/nbreq` uses a temporary copy of GDS's
source/manifest/lock, refreshes that graph through Cargo metadata, verifies the requested NBReq
path, then builds with `--locked`. It needs no source or permanent lock edit to exercise local
winpoll 0.1.1. Use the existing `-SkipCopy` workflow to prepare an artifact without replacing the
installed DLL. The narrow NBReq fix is mirrored to main separately, preserving unrelated work.

After support/root publication, refresh GDS's normal registry lock to select winpoll 0.1.1 or
a later compatible fixed version. Check the resolved graph; `/httpbackend ureq`'s current Delphi
log is not proof of backend selection. That loader fix remains with the GDS session. Record the
rebuilt DLL hash and exact deployed Wine/prefix, then repeat external-client startup, remote scan
retrieval and shutdown. This task has not replaced the installed DLL or performed those GDS steps.

## Final commit, packages and handoff checkpoint

The fix is committed as **`149450d6543376cdb8255b55d7eb286e98207f42`**, branch
`codex/wine-dns-adapters`. That worktree is clean. Its scoped implementation, diagnostic tool
and security wording are mirrored into `C:/User/projects/nbreq` for the existing GDS local-build
path. The sync verified all seven preexisting implementation files matched the candidate base
before replacing them and checked 32 other modified tracked files were unchanged. Main's dirty
work, index, previous candidates and installed GDS DLL remain preserved.

[Artifact manifest](evidence/nbreq_wine_dns_artifacts.json) and
[evidence archive](evidence/nbreq-wine-dns-20260910.tar.gz) retain 112 files, including exact
packages, source patch, probes, shim source, original panic, successful gates and failed attempts.
Archive: 4,255,205 bytes; SHA256
`5d438042cc03009ec7f12c58e4d57b11f1008f5807d9d8cb0ffc8b60433ecc4f`.
Every archived file was rechecked against its recorded hash.

| Clean package | SHA256 | Verification |
| --- | --- | --- |
| nbreq-winpoll 0.1.1 | `85d5bb03211b09c1a8914e3766e7a27843af4d9b0f312a7c5d97c6fe578a41b7` | 12,901 bytes / 10 files; x86 package build; all 13 tests from unmodified archive on Windows x64/Rust 1.85. |
| nbreq 0.2.0 | `5c9cb9188d84eab632b4bd9c2084d267dd24779e7b0500a1cad7e430d89b472b` | 334,161 bytes / 82 files; x86 package build with explicit local Darwin and winpoll overrides, locked/offline. |

Both VCS records identify the clean fix commit without a dirty flag. These checks do not close
the registry-only gate. Two initial extracted-helper test attempts inherited the surrounding
workspace and Cargo refused them before compilation. Extracting the same unmodified archive
outside the repository resolved that harness issue; its Rust 1.85 tests passed. The failed logs
are retained. No package source was altered to obtain the result.

## References

Primary references: [GetAdaptersAddresses](https://learn.microsoft.com/en-us/windows/win32/api/iphlpapi/nf-iphlpapi-getadaptersaddresses),
[adapter layout](https://learn.microsoft.com/en-us/windows/win32/api/iptypes/ns-iptypes-ip_adapter_addresses_lh),
[ipconfig source](https://github.com/liranringel/ipconfig/blob/master/src/adapter.rs), and
[Wine adapter packing](https://github.com/wine-mirror/wine/blob/master/dlls/iphlpapi/iphlpapi_main.c).
Current Wine source aligns descriptive strings; actual Wine-5 execution supplies the evidence
here. No Wine implementation source is copied into NBReq.
