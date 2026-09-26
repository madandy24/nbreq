# Portable verified TLS and optional bundled roots

## Checkpoint

2026-09-26: portable verification is implemented and independently reviewed at local commit `b5c8eb0c1a87d25fff945dac52b7ceb87fc6680e`; a test-only fixture repair follows at `8d24e27d31ec78d3687c03734439b9bcb7a6e0e7`. Windows, both Macs, Rust 1.85, package consumers and advisory/license checks are complete with the qualifications below. Native Win32 live IMAPS/SMTP checks pass. Linux and actual Wine execution remain unrun: automatic approval review rejected the Linux source upload and the user approval question is still pending. Main remains the released baseline. This checkpoint is not release or Wine acceptance.

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

Implemented API: public non-exhaustive `TlsTrust::{Platform, SuppliedRootsOnly, BundledMozilla}`, selected by `EngineConfig::with_tls_trust`, with a `tls_trust()` getter. Existing `with_additional_tls_root_certificate` adds DER roots to the selected source. Variants remain present without optional features so construction can return an explicit unsupported error. `bundled-roots` enables native support and the optional bundle dependency; it is not a default feature. No Rustls types enter the public NBReq API.

## Work sequence

| Stage | Deliverable | State / acceptance |
| --- | --- | --- |
| P0 | Freeze policy and API, inspect baseline, confirm hosts | Complete: accepted API below independently reviewed; Linux, Intel Mac and Apple Silicon bridges verified online. |
| P1 | Compiling scaffold and meaningful failing tests | Complete: clean compile followed by three expected runtime Unsupported failures for trusted HTTPS/direct TLS/STARTTLS. Exact source copies/hashes and raw logs preserved in `red-snapshot/` and `portable-runtime-red-final.log`; independent review accepted. |
| P2 | Implement modes, feature and focused controls | Complete: bundle-on integration 10/10, feature-off integration 10/10, offline verifier/backend proofs 3/3, focused strict Clippy and formatting pass. Independent code/API/docs/tests review has no remaining blocker. |
| P3 | Frozen Windows and actual Wine proof | Native Win32 passes 10 integration + 3 policy proofs and verified live IMAPS/SMTP STARTTLS. Wine pending explicit approval for source/binary/helper uploads after automatic approval review rejected Linux source transfer. Preserve known platform-mode Wine failures separately. |
| P4 | Native platform, feature, MSRV and consumer checks | Windows and both Macs completed; native Linux pending upload approval. Windows full 30-step parallel gate passes; additional bundle-only parallel run hit an existing 150 ms HTTP fixture assumption, preserved separately. Serial bundle-only/MSRV checks pass. Both Macs pass full gates plus final affected-fixture checks. Final packaged tests and independent consumers pass. |
| P5 | Documentation, evidence and checkpoint | User guidance, examples, license report and GDS handoff complete. Permanent source-bound native evidence retained; Linux/Wine appendix and final acceptance remain pending. No push, merge or publication. |

## Proof requirements

Regular tests use bounded local generated fixtures and public APIs, without a test-support verifier or a public network dependency. Include HTTPS/direct TLS/STARTTLS, spawned/manual engines and TLS1.2/1.3 without an unnecessary full Cartesian product. Pair invalid controls with a working trusted positive. Cover wrong DNS/IP identity, expiry, invalid purpose/signature, unrelated root rejection, multiple/additional roots, immutable cloned configuration, separate Engine trust and redirects. Preserve platform-default tests separately.

An offline captured public certificate chain at a fixed evaluation time can prove real bundle acceptance and rejection by an unrelated supplied-only trust store without relying on external network availability. Any such fixtures must retain public provenance/hashes and not contain private keys. Live probes supplement, not replace, deterministic coverage. Preserve cancellation/deadline/accounting regression evidence for the shared TLS machinery.

Source, dependency lockfiles, compiler/architecture, executable hashes, command/result logs and host identities must be tied to final artifacts. Distinguish synthetic/native/Wine/live evidence, diagnostic execution from successful validation, and actual successes from expected failures or unavailable infrastructure. Failed earlier attempts remain labeled accurately.

## Findings and progress

The change adds only `webpki-roots 1.0.9` to the resolved dependency graph; existing Rustls/verifier versions are unchanged. Its Rust 1.70 minimum is below NBReq's Rust 1.85 minimum. Feature and minimum-version checks ran as recorded below.

The three bridge preflights ran on Linux x86_64 (Wine 5.0, Ubuntu package 5.0-3ubuntu1), Intel macOS x86_64 and Apple Silicon macOS arm64. Host identities and 31 raw receipts are preserved. Bridge connectivity worked; the outstanding assistance is explicit Linux upload approval after automatic approval review rejected that action.

The first green integration attempt was 8/10: unrelated synthetic CAs had the same default distinguished name, so the rejection classified as a bad signature instead of the intended unknown issuer. Giving fixture CAs distinct names corrected that test setup; both the failed attempt and final green logs are retained. Production verification was not weakened. Fixed-time captured public-chain checks exercise the same verifier helper as production, prove real Mozilla anchor use, and retain wrong-name/expiry rejection plus supplied-only exclusion.

The backend capability check prevents a test/unsupported backend from silently ignoring explicit trust configuration. Platform-mode additional roots are parsed strictly too. The optional root dependency is the only dependency graph addition. The license report was regenerated with the CI-pinned cargo-about 0.9.1 and all workspace features.

Frozen b5c8 source archive: 154 regular files, 497,586 bytes, SHA256 `718a4bf1f6cea8b0b24d130d174134bb51fc8f95c4f6d79a03c03234ce5d432a`; manifest `c5b8eaad19d49c5b9801eb18adb2401f5927516661670877e5e7d6322478277b`. Win32 archive: 6,603,796 bytes, SHA256 `4cc1f1e2059f941f4063e8af5c15e1fe1ed313f17949fa47cc3565f020bff49f`; all three executables are PE I386. Both inventories independently reviewed before transfer; Mac uploads accepted, Linux transfer rejected by automatic approval review and not retried without user approval.

Packaged b5c8 consumer gate passes: 112 regular package files, 41 relative documentation links, clean VCS identity, 10 integration + 3 policy proofs, independent bundle-disabled/enabled consumers. Package SHA256 `5e4acd862b58b77d3e801427757346a7efe47a1106b024e506842a318232dd36`. Cargo-audit 0.22.2 reports no unignored advisories or warnings against RustSec DB `e2111519ba6d14a5da59a7b2e5c8083ae8a37c01` (2026-09-25); the existing development-only `time` parsing exception remains unchanged and the affected parsing feature is absent.

Initial Windows broad-gate failure: `split_tls_reader_remains_usable_after_writer_finishes` reported strict truncation. The old fixture processed a client close with its last handshake flight, then mistook Rustls's already-authenticated read EOF for raw transport closure and omitted its response. Thirty unchanged focused repetitions passed but did not clear the failure; deterministic proof and the repair below did. Production truncation checks remain strict. Raw failed gate and retry logs are retained.

Fixture repair checkpoint: deterministic isolated and repository-locked runtime reds confirm the coalesced-final-flight defect; raw transport EOF remains a passing rejection control. The shared fixture now drains/checks existing TLS state before reading the socket. Permanent regressions pass 2/2, affected suites 22/22 and 5/5, strict Clippy and formatting pass, independent review approved. This is a test-only correction; production TLS and dependency hashes are unchanged. Final Mac verification combined the passed full gates with both affected suites under native-only/all-features/Rust 1.85 and strict lint/format, enforcing the exact source-manifest delta. Windows reran its full gate and the final package includes and executes the regression.

Both b5c8 Mac native gates passed all 30 verification steps, bundle-only tests, Rust 1.85 all-feature tests and live-probe lint/build. Intel verified IMAPS and SMTP STARTTLS live; Apple Silicon verified IMAPS but SMTP hit a read inactivity timeout, retained as an unavailable live result rather than a certificate-verification success. Windows PE I386 live results remain successful. Linux/Wine remains pending upload approval.

Final Windows qualification: all 30 parallel verification steps pass on 8d24. The extra bundle-only run failed `total_and_inactivity_timeouts_close_the_stalled_socket` after its expected timeout-category assertions passed. This unchanged plain-HTTP fixture starts a 150 ms deadline at admission but later demands a request-arrival event that is not guaranteed before that deadline. The original log cannot identify the loop iteration or socket phase. Ten isolated repeats and the 16-test serial executable pass; the full bundle-only suite and Rust 1.85 all-feature suite then pass with `RUST_TEST_THREADS=1`, followed by strict probe Clippy/release. Preserve the parallel failure and serial qualification: this is controlled scheduling evidence, not a repaired parallel-test stability claim.

Final 8d24 package: 113 files and 41 relative links verified, 10 portable integration + 3 policy + 5 TLS API tests pass, independent bundle-disabled/enabled consumers pass. Package SHA256 `964bd7855f3067de8a71a7bdae94d2efa2c30f527df67ead67a9ec4d7d171b55`; consumer lock is byte-identical to the audited earlier consumer. Windows stable compiler is 1.97.1; both Mac stable compilers are 1.98.0; minimum-version checks use 1.85.0.

## Retained evidence and next actions

Native checkpoint archive: [nbreq-portable-tls-native-20260926.tar.gz](evidence/nbreq-portable-tls-native-20260926.tar.gz), 419 regular members, 2,434,587 bytes, SHA256 `1c55af61848529b848dacaed9b4ecd4ef6aa78b12cbeb7863880ca61365db321`. It retains source freezes, packaged crates, runtime red/green proofs, failed attempts, host receipts, audit/license identities and qualified result records. Compiled executables stay in the local lab with their frozen manifest/hashes. [Artifact summary](evidence/nbreq_portable_tls_artifacts.json).

1. Obtain the pending Linux upload approval and reconfirm the default bridge targets the intended Linode test host (observed `gds-srv-test2`, saved session `gds-client-01i linode`), especially after another reboot. Use the reviewed final source archive `nbreq-portable-tls-8d24e27d31ec-source.tar.gz` (SHA256 `834fd233505693877a764db6ff84ff5c875a7fbdde2aec52e66ed95dfb90ce61`) for native Linux; run the full `native_gate.py` there. The final source differs from the already reviewed first source only in the fixture/regression and run helpers.
2. Transfer the frozen Win32 archive above and `prepare_remote.py`, validate/extract to a new `/tmp` directory, and run `run_win32.py --wine /usr/bin/wine --out <new-result-directory>`. It verifies the existing private Win32 prefix and approved app-local ProcessPrng shim before 10 portable tests, 3 offline/policy proofs and unauthenticated IMAPS/SMTP live checks. It must not modify Wine globally or silently use platform/bypass fallback. Preserve explicit execution/version/architecture and all failures.
3. Retrieve/review those logs, add an evidence appendix, then close P3/P4 if accepted. Keep ARM's live SMTP timeout and the Windows parallel HTTP-test stability item visible; neither is a Wine success.
4. Use the separate [GDS integration handoff](nbreq_portable_tls_gds_handoff.md) when that work is authorized. GDS must select the new trust mode and remove its existing explicit HTTP verification bypass for verified requests. GDS startup/remote-scan/shutdown tests remain a separate integration gate.
