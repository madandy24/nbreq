# Portable verified TLS and optional bundled roots

## Checkpoint

2026-09-26: portable verification is implemented and independently reviewed at local commit `b5c8eb0c1a87d25fff945dac52b7ceb87fc6680e`, based on Wine investigation checkpoint `a673ab5d2a876eaba3cbe1fcbde34048e2634fa5`. Native 32-bit tests/live probes and packaged consumers pass. Broader native gates are running; an existing TLS fixture close-ordering failure is under deterministic investigation. Linux upload authorization is pending following automatic approval rejection. Main remains the released baseline. This plan tracks the remedy and its separate acceptance evidence.

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
| P3 | Frozen Windows and actual Wine proof | Native Win32 passes 10 integration + 3 policy proofs and verified live IMAPS/SMTP STARTTLS. Wine pending explicit approval for source/binary/helper uploads after automatic approval review rejected Linux source transfer. Preserve known platform-mode Wine failures separately. |
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

Frozen b5c8 source archive: 154 regular files, 497,586 bytes, SHA256 `718a4bf1f6cea8b0b24d130d174134bb51fc8f95c4f6d79a03c03234ce5d432a`; manifest `c5b8eaad19d49c5b9801eb18adb2401f5927516661670877e5e7d6322478277b`. Win32 archive: 6,603,796 bytes, SHA256 `4cc1f1e2059f941f4063e8af5c15e1fe1ed313f17949fa47cc3565f020bff49f`; all three executables are PE I386. Both inventories independently reviewed before transfer; Mac uploads accepted, Linux transfer rejected by automatic approval review and not retried without user approval.

Packaged b5c8 consumer gate passes: 112 regular package files, 41 relative documentation links, clean VCS identity, 10 integration + 3 policy proofs, independent bundle-disabled/enabled consumers. Package SHA256 `5e4acd862b58b77d3e801427757346a7efe47a1106b024e506842a318232dd36`. Cargo-audit 0.22.2 reports no unignored advisories or warnings against RustSec DB `e2111519ba6d14a5da59a7b2e5c8083ae8a37c01` (2026-09-25); the existing development-only `time` parsing exception remains unchanged and the affected parsing feature is absent.

Windows broad-gate failure: `split_tls_reader_remains_usable_after_writer_finishes` reported strict truncation. The existing fixture can process a client close with its last handshake flight, then mistake Rustls's already-authenticated read EOF for raw transport closure and omit its response. Thirty unchanged focused repetitions passed but do not clear the failure. An isolated deterministic proof and minimal fixture repair are required; production truncation checks must remain strict. Raw failed gate and retry logs are retained.

Fixture repair checkpoint: deterministic isolated and repository-locked runtime reds both confirm the coalesced-final-flight defect; raw transport EOF remains a passing rejection control. The shared fixture now drains/checks existing TLS state before reading the socket. Permanent regressions pass 2/2, affected native-only suites 22/22 and 5/5, strict Clippy and formatting pass, independent review approved. This is a test-only correction; production TLS and dependency hashes are unchanged. Final verification will combine the already passed full Mac gates with targeted affected-suite checks on the repaired fixture, rerun the interrupted full Windows gate, and repackage with the fixture regression included.

Both b5c8 Mac native gates passed all 30 verification steps, bundle-only tests, Rust 1.85 all-feature tests and live-probe lint/build. Intel verified IMAPS and SMTP STARTTLS live; Apple Silicon verified IMAPS but SMTP hit a read inactivity timeout, retained as an unavailable live result rather than a certificate-verification success. Windows PE I386 live results remain successful. Linux/Wine remains pending upload approval.
