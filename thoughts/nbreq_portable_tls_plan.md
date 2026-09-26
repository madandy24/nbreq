# Portable verified TLS and optional bundled roots

## Checkpoint

2026-09-26: owner accepted explicit WebPKI verification, optional Mozilla roots supplied by `webpki-roots`, and application-supplied additional roots. Implement in the existing clean `codex/nbreq-smtp` worktree from `a673ab5d2a876eaba3cbe1fcbde34048e2634fa5`. Main remains the released baseline. The preceding [Wine investigation](nbreq_tcp_tls_wine_followup.md) is complete; this plan tracks the production remedy and its separate acceptance evidence.

Root coordinates plans, source freezes, bridge queues and evidence. Sol worker owns API/backend/dependencies and user documentation; Sol test worker owns regression fixtures/tests; Astra reviews design, code, controls and results, with implementation/fix/re-review cycles until findings are resolved. Local evidence lab: `C:/User/projects/nbreq/target/portable-tls-20260926/`.

## Accepted scope and policy

- Platform verification remains the default, including its current additional-root behavior. Enabling a Cargo feature alone never changes the selected trust policy.
- Add an explicit supplied-root-only WebPKI mode and an explicit Mozilla-bundle WebPKI mode that also accepts custom roots. Both serve HTTPS, direct TCP TLS and STARTTLS through the shared native TLS configuration.
- The optional, non-default `bundled-roots` feature depends directly on `webpki-roots` with a normal compatible version requirement. Use its compact trust anchors; do not introduce a separate NBReq root-data crate or copy complete public root certificates unnecessarily.
- Additional DER certificates supplement the selected trust source. Supplied-only mode imports no platform or bundled trust. Empty supplied-only and malformed root configurations fail at construction; unavailable feature/backend combinations fail explicitly.
- The portable modes retain chain signatures, certificate purpose, hostname/IP and validity checks. They do not inherit platform enterprise trust/distrust or automatic OS revocation retrieval. No automatic fallback, Wine detection, silent verification bypass or system trust-store mutation.
- Root updates may be adopted by downstream applications without a corresponding NBReq release while the dependency API remains compatible. A locked application must update its dependency and rebuild/deploy to refresh compiled roots. Ordinary GDS releases are a refresh opportunity; urgent distrust events may require earlier updates.
- GDS integration, deployment, Wine upgrades, publication and authenticated mail sending are outside this implementation pass. No automatic root downloader, CRL distribution/refresh API or new TLS protocol is included.

## API decision

Proposed implementation: public non-exhaustive `TlsTrust::{Platform, SuppliedRootsOnly, BundledMozilla}`, selected by `EngineConfig::with_tls_trust`, with a `tls_trust()` getter. Existing `with_additional_tls_root_certificate` adds DER roots to the selected source. Variants remain present without optional features so construction can return an explicit unsupported error. `bundled-roots` enables native support and the optional bundle dependency; it is not a default feature. No Rustls types enter the public NBReq API.

## Work sequence

| Stage | Deliverable | State / acceptance |
| --- | --- | --- |
| P0 | Freeze policy and API, inspect baseline, confirm hosts | Complete: accepted API below independently reviewed; Linux, Intel Mac and Apple Silicon bridges verified online. |
| P1 | Compiling scaffold and meaningful failing tests | Complete: clean compile followed by three expected runtime Unsupported failures for trusted HTTPS/direct TLS/STARTTLS. Exact source copies/hashes and raw logs preserved in `red-snapshot/` and `portable-runtime-red-final.log`; independent review accepted. |
| P2 | Implement modes, feature and focused controls | Complete: bundle-on integration 10/10, feature-off integration 10/10, offline verifier/backend proofs 3/3, focused strict Clippy and formatting pass. Independent code/API/docs/tests review has no remaining blocker. Broader frozen gates follow. |
| P3 | Frozen Windows and actual Wine proof | Pending: identical Win32 artifacts on native Windows and Wine5; public API positives and negatives; explicit bundled-root live probes to owner-controlled mail host without credentials or mail. Preserve known platform-mode Wine failures as a separate limitation. |
| P4 | Native platform, feature, MSRV and consumer checks | Pending: appropriate Windows/Linux/Intel Mac/Apple Silicon regressions, default and optional feature combinations, Rust1.85 and packaged consumer evidence. Reuse reviewed bounded process supervision. |
| P5 | Documentation, evidence and checkpoint | Pending: user-facing configuration/update guidance, GDS handoff, reviewed permanent evidence, clean local commit. No push, merge or publication. |

## Proof requirements

Regular tests use bounded local generated fixtures and public APIs, without a test-support verifier or a public network dependency. Include HTTPS/direct TLS/STARTTLS, spawned/manual engines and TLS1.2/1.3 without an unnecessary full Cartesian product. Pair invalid controls with a working trusted positive. Cover wrong DNS/IP identity, expiry, invalid purpose/signature, unrelated root rejection, multiple/additional roots, immutable cloned configuration, separate Engine trust and redirects. Preserve platform-default tests separately.

An offline captured public certificate chain at a fixed evaluation time can prove real bundle acceptance and rejection by an unrelated supplied-only trust store without relying on external network availability. Any such fixtures must retain public provenance/hashes and not contain private keys. Live probes supplement, not replace, deterministic coverage. Preserve cancellation/deadline/accounting regression evidence for the shared TLS machinery.

Source, dependency lockfiles, compiler/architecture, executable hashes, command/result logs and host identities must be tied to final artifacts. Distinguish synthetic/native/Wine/live evidence, diagnostic execution from successful validation, and actual successes from expected failures or unavailable infrastructure. Failed earlier attempts remain labeled accurately.

## Findings and progress

The compiling scaffold adds only `webpki-roots 1.0.9` to the resolved dependency graph; existing Rustls/verifier versions are unchanged. Its Rust 1.70 minimum is below NBReq's Rust 1.85 minimum. Full feature and minimum-version checks remain required after implementation. Green implementation is underway following the reviewed runtime-red checkpoint.

The three bridge preflights ran on Linux x86_64 (Wine 5.0, Ubuntu package 5.0-3ubuntu1), Intel macOS x86_64 and Apple Silicon macOS arm64. Host identities and raw receipts are preserved in the local lab/bridge queues. No bridge assistance is currently required.

The first green integration attempt was 8/10: unrelated synthetic CAs had the same default distinguished name, so the rejection classified as a bad signature instead of the intended unknown issuer. Giving fixture CAs distinct names corrected that test setup; both the failed attempt and final green logs are retained. Production verification was not weakened. Fixed-time captured public-chain checks exercise the same verifier helper as production, prove real Mozilla anchor use, and retain wrong-name/expiry rejection plus supplied-only exclusion.

The backend capability check prevents a test/unsupported backend from silently ignoring explicit trust configuration. Platform-mode additional roots are parsed strictly too. The optional root dependency is the only dependency graph addition. The license report was regenerated with the CI-pinned cargo-about 0.9.1 and all workspace features.
