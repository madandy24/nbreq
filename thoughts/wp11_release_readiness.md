# WP11 release-readiness ledger

Status: opened 2026-08-24 after WP10 acceptance. This is a source-grounded checklist for turning the
accepted private implementation into a public crate. It does not authorize publication by itself.

## 1. Accepted foundation

- NBReq / `nbreq` means Non-Blocking Request.
- Native HTTP is the default feature and ordinary constructor; curl is explicit.
- The crate MSRV is Rust 1.85 with Rust 2024 edition.
- Windows 10, exact-source Ubuntu 20.04, controlled Windows x86 GDS, and stock-Wine-5 compatibility
  have named evidence. The public supported-target wording still needs a final freeze.
- The 22-stage verifier covers formatting, minimal/default/native/curl/all-feature builds and tests,
  warning-denied lint, doctests, documentation, and named pressure regressions.
- Public code now enables the `missing_docs` lint; the existing warning-denied all-feature stage
  enforces it.

## 2. Publication blockers and decisions

| Item | Current state | Required WP11 decision/action |
|---|---|---|
| License grant | **Resolved:** Copyright (c) 2026 Cave Rock Software Limited; standard `LICENSE-MIT` and `LICENSE-APACHE`; manifest `MIT OR Apache-2.0` | Recheck packaged license inclusion at the release rehearsal |
| Version | `0.0.0` | Choose the first public version, expected to be an explicitly pre-stable release |
| Registry metadata | **Identity resolved:** planned repository/homepage `https://github.com/madandy24/nbreq`, docs.rs URL, README, keywords, and categories are in the manifest; `publish = false` remains | Create the public remote, verify all URLs, and remove `publish = false` only at the release gate |
| Windows support crate | Default native depends on path-only, unpublished `support/winpoll` / `nbreq-winpoll` | Either publish it as a versioned implementation-detail crate from the same workspace, or choose another packaging boundary that preserves NBReq's safe public API and audited unsafe isolation |
| Curl reference | `curl-pilot` requires a locally patched path override and private feature/API extensions | Upstream the patch, publish and maintain a clearly named implementation-detail fork, or omit curl from the public package until a registry-resolvable solution exists |
| Package contents | Dry-run package includes internal thoughts, experiments, proof scripts, and evidence while excluding both nested path packages | Add a deliberate include/exclude policy and inspect the exact `.crate`; retain only material useful to consumers and required licenses/notices |
| Empty `ffi` feature | Manifest exposes `ffi = []` but no code uses it | Remove it before the first public version or define its promised contract; do not reserve a meaningless stable feature |
| Security contact | No `SECURITY.md` or public reporting route | Add supported-version and private-reporting policy after repository/contact choice |
| CI | Portable CI intentionally deferred in P10-04 until a genuine public repository exists | Add the existing verifier to Windows and Linux CI without replacing named target-host/Wine/GDS evidence |

The two path-dependency items are release blockers, not runtime defects. Cargo's package list omits
the nested support and curl packages, so publishing the current root archive cannot reproduce the
accepted feature matrix from crates.io alone.

## 3. Dependency-license audit

`cargo metadata --locked` on the accepted lock graph reports an SPDX license or license file for
every package. The graph is permissive: MIT, Apache-2.0, ISC, BSD, Unicode-3.0, Unlicense, Zlib,
CDLA-Permissive-2.0, and compatible disjunctions/conjunctions. `r-efi` offers permissive alternatives
alongside LGPL rather than requiring LGPL; `ring` is Apache-2.0 AND ISC. No dependency is presently
unclassified.

This inventory includes dev, target-specific, and optional dependencies. Before packaging, generate
an exact normal-feature and all-feature notice/license report from the final release manifest, audit
the locally modified MIT curl source if it remains, and retain any required upstream notices. Do
not copy this summary into legal files as a substitute for the exact release-tree report.

## 4. Documentation audit

The Rust API already has strong type-level lifecycle documentation. Strict `missing_docs` found nine
undocumented metrics accessors; the WP11 opening slice documents them and enables the lint at crate
scope. The README previously led with the curl pilot despite native being ordinary; it now leads
with a native quick start and links a consumer guide.

The first guide covers:

- unique Engine and cloneable Client ownership;
- blocking, callback, direct waiter, and manual-drive families;
- individual and Engine-domain cancellation;
- streamed response plus fixed/chunked upload ownership and backpressure;
- GUI dispatch and DLL/FFI shutdown order;
- limits, pools, metrics, TLS defaults, and explicit backend selection.

Remaining public documentation includes a compact feature/platform matrix, error-handling recipes,
security policy, release/semver policy, and generated API-documentation links after repository and
docs.rs metadata exist.

## 5. Proposed WP11 sequence

1. **WP11.0 — public surface and guide audit:** enforce missing docs, add native-first onboarding,
   and freeze this ledger.
2. **WP11.1 — identity and package topology:** holder, dual license, and repository identity are
   resolved; decide the version, resolve both path dependencies, remove or define empty features,
   and freeze packaged contents.
3. **WP11.2 — security and API audit:** review unsafe boundary, panic/callback containment, secret
   redaction, TLS policy, denial-of-service limits, semver surface, and reporting policy.
4. **WP11.3 — release automation:** add real repository CI, exact license/notice generation,
   `cargo package`/clean-consumer rehearsals, and the longer lifecycle soak tier.
5. **WP11.4 — alpha publication:** publish only from a clean exact commit after Windows/Linux gates
   and a clean external consumer build. Keep curl and GDS ureq rollback during observation.

Post-WP11 DNS/TCP facades remain separate follow-up work and must not expand the first HTTP release
surface while these blockers are being closed.
