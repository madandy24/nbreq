# WP11 release-readiness ledger

Status: opened 2026-08-24 after WP10 acceptance. This is a source-grounded checklist for turning the
accepted private implementation into a public crate. It does not authorize publication by itself.

## 1. Accepted foundation

- NBReq / `nbreq` means Non-Blocking Request.
- Native HTTP is the default feature and ordinary constructor. The pre-release curl reference is
  deliberately omitted from the first public package.
- The crate MSRV is Rust 1.85 with Rust 2024 edition.
- Windows 10, exact-source Ubuntu 20.04, controlled Windows x86 GDS, and stock-Wine-5 compatibility
  have named evidence. The public supported-target wording still needs a final freeze.
- The release verifier covers formatting, minimal/default/native/all-feature builds and tests,
  warning-denied lint, doctests, documentation, and named pressure regressions. Removing the two
  retired curl stages makes an ordinary run 20 stages.
- Public code now enables the `missing_docs` lint; the existing warning-denied all-feature stage
  enforces it.

## 2. Publication blockers and decisions

| Item | Current state | Required WP11 decision/action |
|---|---|---|
| License grant | **Resolved:** Copyright (c) 2026 Cave Rock Software Limited; standard `LICENSE-MIT` and `LICENSE-APACHE`; manifest `MIT OR Apache-2.0` | Recheck packaged license inclusion at the release rehearsal |
| Version | **Resolved:** root and implementation-detail support crate are `0.1.0` | Keep 1.0 gated on post-publication production observation and API stability |
| Registry metadata | **Identity resolved:** planned repository/homepage `https://github.com/madandy24/nbreq`, docs.rs URL, README, keywords, and categories are in the manifest; `publish = false` remains | Create the public remote, verify all URLs, and remove `publish = false` only at the release gate |
| Windows support crate | **Topology resolved:** `nbreq-winpoll` is `0.1.0`, crates.io-scoped, explicitly described as an implementation detail, and the root uses path + registry version | Publish and verify this crate immediately before `nbreq`; do not market or stabilize it as a direct consumer API |
| Curl reference | **Resolved for 0.1.0:** feature, dependency, patch, public variant, and verifier stages are absent from the public package graph | Retain the accepted pilot in history/evidence; GDS rollback is ureq; reconsider only after an upstreamable registry-resolvable binding exists |
| Package contents | **Resolved:** explicit include list retains source, public tests/examples/guide, README, security policy, and both licenses; it excludes thoughts, experiments, proof tools, archived curl source/tests, and comparison examples | Inspect both final `.crate` archives and clean-consumer builds again at publication |
| Empty `ffi` feature | **Resolved:** removed before 0.1.0 | Add a future FFI feature only with an implemented and documented contract |
| Security contact | **Policy resolved:** packaged `SECURITY.md` selects GitHub private vulnerability reporting, latest-0.x support, and no public/secret-bearing initial reports | Enable private vulnerability reporting on the real repository before release; until then the route is not live |
| CI | **Prepared locally:** a least-privilege GitHub Actions workflow runs the complete verifier on stable Windows, stable Ubuntu, and Ubuntu/Rust 1.85, with separate RustSec and license-report gates | Create the genuine public repository, run the workflow there, and retain named target-host/Wine/GDS evidence for claims portable CI cannot reproduce |

The remaining topology gate is publication order, not an architecture defect: publish and verify
`nbreq-winpoll` first, then package `nbreq` so its normalized manifest resolves version `0.1.0` from
crates.io. Both crates remain unpublished during WP11 review. Curl no longer participates in the
public resolution graph.

The 2026-08-24 rehearsal confirms that boundary. `cargo package --list` for each crate contains only
the frozen files. Offline `cargo package -p nbreq-winpoll --allow-dirty` produced and compiled an
eight-file support archive. Root packaging then stopped before archive creation because
`nbreq-winpoll 0.1.0` is not yet present in the crates.io index. That is the intended fail-closed
publish order; the root archive and external clean-consumer build remain a post-support-publication
release rehearsal rather than being simulated with a local registry patch.

## 3. Dependency-license audit

`cargo metadata --locked` on the accepted lock graph reports an SPDX license or license file for
every package. The graph is permissive: MIT, Apache-2.0, ISC, BSD, Unicode-3.0, Unlicense, Zlib,
CDLA-Permissive-2.0, and compatible disjunctions/conjunctions. `r-efi` offers permissive alternatives
alongside LGPL rather than requiring LGPL; `ring` is Apache-2.0 AND ISC. No dependency is presently
unclassified.

The checked-in `THIRD_PARTY_LICENSES.html` is generated from the locked non-development graph for
both supported release targets with all release features enabled. Pinned `cargo-about 0.9.1`, the
accepted-license policy in `about.toml`, and the deterministic `about.hbs` template reproduce it;
CI regenerates the report and rejects byte-level drift. This exact report, rather than this summary,
is included in the root package.

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

Remaining public documentation includes a compact feature/platform matrix, release/semver policy,
and generated API-documentation links after repository and docs.rs metadata exist. Error/TLS
handling recipes and the packaged security policy are now present.

## 5. Proposed WP11 sequence

1. **WP11.0 — public surface and guide audit:** enforce missing docs, add native-first onboarding,
   and freeze this ledger.
2. **WP11.1 — identity and package topology:** holder, dual license, repository identity, `0.1.0`,
   native-only public graph, versioned support-crate boundary, feature cleanup, and packaged contents
   are resolved. Publication itself remains gated.
3. **WP11.2 — security and API audit:** accepted source review in
   `wp11_security_api_audit.md`: unsafe remains isolated, panic/callback containment holds, raw TLS
   diagnostics become structured payload-free categories, resource ceilings remain owner-bounded,
   evolving policy types become non-exhaustive, and `SECURITY.md` defines reporting/support policy.
   Current-advisory scanning and complete Windows/Linux verification pass at `de21963`.
4. **WP11.3 — release automation:** add real repository CI, exact license/notice generation,
   `cargo package`/clean-consumer rehearsals, and continued lifecycle soak tiers.
5. **WP11.4 — initial publication:** publish only from a clean exact commit after Windows/Linux gates
   and a clean external consumer build. Keep GDS ureq rollback during observation.

Post-WP11 DNS/TCP facades remain separate follow-up work and must not expand the first HTTP release
surface while these blockers are being closed.

The WP11.1 implementation passes the complete revised 20-stage Windows verifier in 70.532 seconds.
This records package-topology compatibility with the existing code gates; it is not the final
cross-platform release run.

The WP11.2 implementation passes the complete 20-stage Windows verifier in 67.232 seconds after a
current 1,225-advisory RustSec scan and ten additional adversarial-suite repetitions. Exact-source
Rust 1.85 rejected the initially selected fixed `time 0.3.47` because that dev-only fixture
dependency now requires Rust 1.88; 0.3.46 has the same MSRV. The graph therefore keeps
Rust-1.85-compatible `time 0.3.45`; its RFC-2822 parser advisory is unreachable because the
parsing feature is not compiled and rcgen only
constructs certificate dates. That dev-only exception and the two Hickory exceptions have
source-locked rationales in `wp11_security_api_audit.md` and `SECURITY.md`; eliminating the
wire-only Hickory dependency is early post-WP11 work. Final commit `de21963` passes the complete
20-stage Windows verifier in 72.057 seconds; its authenticated 563,206-byte archive (SHA-256
`115E37BCF017AEECB4595BEC9A818E8BCB5E791C66CEAC5BAAF90926B8B3448A`) passes all 20 stages
offline on Ubuntu 20.04.6 / Rust 1.85.0 in 265.644 seconds. WP11.2 is accepted.

The local WP11.3 automation checkpoint is recorded in `wp11_release_automation.md`. It prepares the
real-repository workflow, reviewed advisory policy, exact generated license report, and package
contents, and passes the complete 20-stage Windows verifier in 29.940 seconds. GitHub-hosted runs,
private vulnerability reporting, registry resolution, and publication remain deliberately open.

## 6. First realistic native observation

The controlled Windows/GDS run recorded in `wp11_native_soak_evidence.md` selected native at
00:57:34 and closed normally at 11:12:15: 10 hours 14 minutes 41 seconds. Both real long-poll
channels remained active, the authenticated website was still healthy at close, and 40 application
responses mapped to 40 successful POST acknowledgements. There was no unexpected HTTP/transport
failure; final individual cancellation joined the two WebRPC owners in 1 ms and 0 ms, and no
process remained. This supports `0.1.0` and closes one realistic observation item without claiming
multi-platform soak, fleet readiness, performance, or 1.0 stability.
