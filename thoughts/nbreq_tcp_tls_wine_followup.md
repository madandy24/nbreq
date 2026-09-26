# Standalone TCP TLS on Wine: follow-up

## Active investigation checkpoint — 2026-09-26

Owner requested investigation before the next NBReq release, including whether GDS's certificate-verification disable originated from this behavior. Wine5 is representative of older GDS installs; some deployments use newer versions. An upgrade or continued application-level disable is a possible owner fallback, not the assumed library fix.

| Stage | Work | State |
| --- | --- | --- |
| W1 | Reconfirm runtime and trace GDS policy/history | Complete: Linux `gds-srv-test2` reports Wine5.0 Ubuntu5.0-3ubuntu1 with x86/x64 packages; existing private Win32 test prefix present. GDS history traced read-only; original bypass rationale is unrecorded. |
| W2 | Isolate exact CryptoAPI failure | Complete: confirmed structure-size and revocation-policy failures; compact key-usage fixture removes its separate wrong-usage error. Supplying an empty signed CRL still leaves Wine5's offline-revocation failure. |
| W3 | Select supported remedy | Owner accepted explicit portable WebPKI trust with optional `webpki-roots` Mozilla bundle and custom roots. Platform remains default. Implementation and new evidence tracked in the [portable TLS plan](nbreq_portable_tls_plan.md); diagnosis alone still does not establish compatibility. |
| W4 | Regression proof and handoff | Investigation complete; independent reviewer accepted source, native/Wine results, conclusions and permanent archive. Revoked/untrusted controls and failed positive controls remain explicit. Production compatibility awaits WQ-01 and its implementation/validation. |

Worktree `target/worktrees/nbreq-smtp`, starting checkpoint `fcd462e`; main remains untouched. Root owns plan/remote queues/evidence, Sol worker owns standalone diagnostic source, Sol test agent traces GDS policy/history read-only, Astra reviewer reviews design and results. New evidence lab: `C:/User/projects/nbreq/target/wine-tls-trust-20260926/`. No GDS configuration, system trust store or deployed binaries are changed during isolation.

The initial structure-size hypothesis is now experimentally confirmed for the tested Wine5 runtime. It explains additional-root construction only. Default-trust verification has a separate policy failure, detailed below. No library or GDS production code has changed.

## Confirmed isolation — 2026-09-26

The standalone `tools/wine-tls-probe` is an unpublished nested workspace pinned to the production verifier versions (`rustls`0.23.45, `rustls-platform-verifier`0.7.0). It records raw API results and tests deliberately invalid certificates. Exit zero means the diagnostic executed, not that certificate verification passed. Generated keys and stores remain in memory; ordinary CryptoAPI URL retrieval and caching can still occur under the existing flags.

First frozen manifest: `12534912a8382fbacc4c378a8cb088e5e639855a9c359326c95134e2d67e3035`. The identical I386 executable, SHA256 `d08c9ea5391a3e91a116fe0864520f86249e7150e7a9036564675e71716df08b`, ran on native Windows11 and Wine5.0 (Ubuntu5.0-3ubuntu1) using the existing private Win32 prefix and separately identified ProcessPrng shim. Native and Wine raw results are `native-12534912a838/` and `wine-12534912a838/` in the evidence lab.

| Observation | Native Win32 reference | Actual Wine5 result | Conclusion |
| --- | --- | --- | --- |
| Chain-engine configuration, 52 bytes | Constructs | Fails `0x80070057` with either a root store or null root | Wine5 rejects the newer structure size; not a malformed supplied certificate. |
| Same configuration, 48 bytes | Constructs and private-root positive control passes | Constructs | Construction compatibility only; later validation still fails. The 40-byte case is constructor-only and omits exclusive-root semantics. |
| Captured public mail-server chain, exact name | Pinned dependency and raw API accept | Chain building succeeds; sole aggregate trust error is `0x01000000` (offline revocation). SSL policy returns `0x800B010E` (`CERT_E_REVOCATION_FAILURE`). | Independent of the additional-root constructor failure. Same public DERs and evaluation time were used on both systems. |
| Public chain, wrong name | Rejected | Same earlier revocation failure | Wine hostname rejection is masked; this is not a successful negative-control proof. |
| Generated private root with 48-byte engine | Valid accepts; wrong name and expired reject | Root element also reports `CERT_E_WRONG_USAGE` | A distinct fixture/key-usage encoding interaction is being isolated; do not infer ECDSA is unsupported. |

The public chain was captured from owner-controlled `cavesvr3.caverock.com:993`; OpenSSL on Linux independently verified the chain and hostname. No login or mail sending was involved. The leaf and two supplied intermediates are retained as public DER files with SHA256 hashes and capture time; evaluation time was fixed at Unix `1790385456`. A first capture verified the chain without an explicit hostname check, so the later explicit hostname check is the authoritative positive reference. Only sanitized public-certificate capture text belongs in permanent evidence; raw `s_client` session-ticket output is excluded.

Source review corroborates the runtime evidence. [Wine5 chain.c](https://raw.githubusercontent.com/wine-mirror/wine/wine-5.0/dlls/crypt32/chain.c), SHA256 `fd7e9db8bf3002e5518e6775454c391ef63c3f2ca858344d92960b5a30d5c5f0`, accepts only the old configuration sizes (lines232–236). Its SSL policy reads `HTTPSPolicyCallbackData.fdwChecks`, but does not use `CERT_CHAIN_POLICY_PARA.dwFlags` (lines3453–3592). The dependency supplies `CERT_CHAIN_POLICY_IGNORE_ALL_REV_UNKNOWN_FLAGS` (`0xF00`), which [Windows documents](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/ns-wincrypt-cert_chain_policy_para) as permitting unavailable revocation information. Wine5 nevertheless rejects the offline-revocation bit. Its alternative `SECURITY_FLAG_IGNORE_REVOCATION` skips both offline and **known revoked** errors, so it is not a suitable compatibility repair.

Independent review accepted the first frozen source, archive, native controls and Wine observations as diagnostic evidence. It explicitly did not accept a Wine compatibility claim. Follow-up retains the original evidence and compares policy flags on the same built chain, records bounded per-certificate details, and isolates the generated root's key-usage encoding without disabling checks.

### Final diagnostic controls

Follow-up manifest `4aadb487b9e9080f044491442eed0e20f99beeb36516eee8a7c71c8bb1afc680` binds executable SHA256 `7917ffe95253aee9b15d02d82d042ff8e465979311babffda7676e89772b2af1`. The same Win32 binary ran original generated controls, a compact-key-usage variant with signed in-memory CRLs, and the captured public chain on Windows11 and actual Wine5. Native/Wine folders are `native-4aadb487b9e9/` and `wine-4aadb487b9e9/`. Retrieved Wine archive SHA256 `cf06cd539805b897c0694af752bb1075fd0991539f9cc242502d42152d95da6f` (8,232 bytes;11 evidence files plus manifest).

- The original rcgen0.13.2 CA encodes keyCertSign/cRLSign as `0303070600`; the compact variant encodes the same permissions as `03020106`. These runs generate new keys/certificates and are not a byte-identical certificate comparison. Native Windows accepts both variants. Wine's generated-root wrong-usage bit disappears with the compact variant, consistent with its source reading the last key-usage byte. This is a fixture compatibility finding, separate from the public RSA chain failure.
- On the **same built public chain**, policy flags `0xF00` and `0` both return Wine `CERT_E_REVOCATION_FAILURE`. Per-element logging locates the sole error at the leaf; all issuer/root element trust errors are zero. Wine provides no per-element revocation-info object, consistent with its source FIXME; null does not mean no check ran.
- A fresh signed empty CRL is accepted by native Windows, with wrong-name and expired controls rejected. Wine still reports offline revocation. Wine5's `cryptnet_main.c` lines1672–1708 explicitly treats a certificate absent from a supplied offline CRL as unavailable revocation information; this attempt did **not** produce a Wine positive control.
- A separate store/engine containing a signed CRL that revokes the leaf returns native `CRYPT_E_REVOKED` and Wine `CERT_E_REVOKED`. A separate untrusted-root case returns `CERT_E_UNTRUSTEDROOT` on both. These are observed specific rejection results, not substitutes for a matching Wine positive or proof that all hostname/trust behavior works. The probe rejected no known-invalid acceptance because none was observed; unavailable positive controls remain explicit failures.

No broad ignore-revocation flag, trust-bit mutation, certificate bypass or persistent root/CRL installation was used. The new diagnostic passes formatting and strict i686 Clippy checks, plus a Rust1.85 Windows x64 compile check. Rust1.85's i686 target was absent and was not installed; the actual native/Wine I386 executable was built using Rust1.97.1. These checks validate the diagnostic, not production Wine compatibility.

### Permanent evidence

[Archive](evidence/nbreq-wine-tls-trust-20260926.tar.gz) and [member/hash index](evidence/nbreq_wine_tls_trust_artifacts.json):102,800 bytes,96 regular members,505,672 uncompressed bytes; SHA256 `3d9ae3c3b01c759d3adb044dcf690318b666c1f828cfe97cb2cf60d54a0e4579`. The packager validates the two frozen source/archive/binary identities, native/Wine result bindings, and the retrieved Wine manifests before writing. It retains both frozen source versions, raw diagnostic logs, public DER certificates, sanitized capture text, exact dependency locks/build metadata, reviewed helper scripts, source-comparison hashes/URLs and filtered bridge receipts.

The permanent bundle excludes executables, build trees, private keys, credentials, GDS source, raw OpenSSL session output and full third-party source files. The ignored local lab retains the exact executables and reviewed upstream sources for follow-up. No failed Wine positive control has been relabeled as a pass.

Final independent Astra review accepted all96 archive members, member/index/source identities, ten completed filtered bridge receipts, diagnostic controls and bounded conclusions. No investigation finding remains open. This acceptance does not approve deployment, a verification-policy change or Wine compatibility. Production NBReq/GDS sources and dependency versions remain unchanged; the new nested diagnostic workspace is unpublished.

### Upgrade is a testable option, not an assumed fix

Read-only upstream release-source comparison is retained as `upstream-wine-release-comparison.json`, including URLs and file hashes. Wine9.0 and Wine10.0 still reject offline revocation without consulting the relevant unknown-revocation policy flag. Wine11.0's SSL policy recognizes the end-certificate unknown-revocation flag, but its header still lacks the modern chain-engine `dwExclusiveFlags` member. No newer Wine release was installed or executed in this investigation. Select a specific candidate and rerun both construction and strict validation before recommending a runtime upgrade; an unqualified "newer Wine fixes it" would overstate the evidence.

## WQ-01: verified support for older Wine — accepted 2026-09-26

Owner accepted the portable option below, refined to include an optional Mozilla bundle through `webpki-roots`, additional custom roots, and supplied-root-only mode. Use a compatible dependency requirement so routine root-data updates do not require a matching NBReq release. Applications must refresh their lockfile and rebuild/deploy to adopt updated compiled roots. See the [implementation plan](nbreq_portable_tls_plan.md) for scope, tests and progress. The following investigation evidence remains historical and is not a claim that the new implementation has passed.

| Option | Benefit | Cost / limitation |
| --- | --- | --- |
| Test and require a specific newer Wine runtime | Retains the platform verifier and its trust policy; no new NBReq trust API | Requires deployment upgrades and runtime proof. Source inspection alone cannot select a supported minimum. |
| Explicit application-supplied-root WebPKI mode, platform default retained | Keeps chain/signature, hostname and expiry validation while avoiding Wine's certificate APIs. Covers HTTPS, direct TLS and STARTTLS through the existing shared configuration. | Application owns root distribution/updates. Does not automatically inherit platform enterprise trust/distrust or Windows revocation retrieval. This is a distinct trust policy, never a silent fallback. |
| Continue application-level verification disable | Preserves existing GDS HTTP behavior | Does not authenticate the server. It is not a verified NBReq Wine support claim and is not recommended as the library remedy. |

Recommendation for a fleet that must retain Wine5: implement and prove the explicit supplied-root WebPKI option, while keeping normal platform verification as the default. The existing private `NativeTlsConfigs::with_test_root` already demonstrates the underlying builder with current dependencies; no new crypto dependency is required for supplied DER roots. Production configuration would belong on `EngineConfig`, which is shared by HTTPS and standalone TCP. Preserve the existing `with_additional_tls_root_certificate` meaning (supplement platform trust), use a distinct supplied-root mode with root-only semantics, reject malformed/empty configuration explicitly, and avoid a per-request downgrade on verification failure. A bundled public-root list is a separate dependency/update decision.

The ordinary Rustls `with_root_certificates` builder does **not** enable revocation checking. Supporting application-supplied CRLs would require additional policy and refresh design; it must not be claimed implicitly. These tradeoffs follow the pinned Rustls0.23.45 builder and [platform-verifier's deployment guidance](https://github.com/rustls/rustls-platform-verifier#deployment-considerations). No production trust-policy change has been made during this investigation.

Acceptance gates for any portable implementation: public-API HTTPS/direct-TLS/STARTTLS positive cases and rejection of unrelated/absent roots, wrong DNS/IP identity, expiry, invalid purpose and invalid signatures; TLS1.2/1.3; spawned/manual engines; isolation between differently configured engines and redirects; existing cancellation/deadline/accounting regressions; identical frozen Win32 binary on native Windows and Wine5, then supported-platform/MSRV checks. Select and document root-update and revocation policy explicitly. Existing test-only WebPKI success is a useful lead, not production acceptance.

GDS integration is separate: selecting a new verified engine mode would not undo its current unconditional HTTP bypass. A GDS session would need to supply the intended roots and change that adapter policy, then retest external-client startup, remote retrieval and shutdown. This investigation leaves GDS untouched.

## Prior frozen evidence

The frozen production commit `bede85c32a41b7099c073e9585517d0ae4027342`
was exercised under Wine 5.0 (Ubuntu 5.0-3ubuntu1) with the existing private
Win32 prefix at `/tmp/nbreq-wine-dns-lab-20260910/prefix`. The run used an
app-local ProcessPrng shim with SHA-256
`92d437dc538ef6ddfae7fc0b2140ec407a6dacfb0c899ec67a6812e84387de86`.
Raw command, host, result and test logs are in
`C:/User/projects/nbreq/target/tcp-tls-20260926/wine-bede85c/` and
`C:/User/projects/nbreq/target/tcp-tls-20260926/wine-diagnostics/`. The
independent constructor, unchanged HTTPS-roots comparison and corrected I/O
filter are in `C:/User/projects/nbreq/target/tcp-tls-20260926/wine-roots/`.

The public TLS suite failed 20/20 under this Wine environment. Nineteen cases
stopped while configuring additional trust roots; the remaining case returned
`CertificateInvalid` for an untrusted certificate where the test expected
`CertificateUnknownIssuer`. All three public API cases also stopped at the
additional-root setup. Both live default-trust mail probes reached TLS
verification and returned `CertificateInvalid`. The
`platform_with_extra_roots` constructor is unchanged from baseline `6ada116`. An
independent i686 constructor check returned `Ok` for both default and extra
root constructors on native Windows. Under this Wine prefix, the default constructor returned `Ok`,
while extra roots returned
`Err(General("Invalid parameter. (os error -2147024809)"))` at the
platform-verifier dependency boundary; a separate WebPKI root store accepted
the same CA. The unchanged HTTPS additional-roots suite passed its malformed
certificate rejection case and failed its other three cases at Engine
additional-root configuration. This narrows the extra-root setup failure but
does not identify which Win32 parameter or Wine behavior caused it, nor
explain the separate default-trust live `CertificateInvalid` results.

Ten standalone-owner tests passed under Wine using a verified WebPKI test CA.
Three registry and six worker tests separately passed lifecycle and accounting
checks. These results do not establish working platform trust or full public
TLS support in this Wine 5.0 prefix. The first I/O filter selected zero tests
and provides no passing evidence; a corrected filter subsequently passed 13
I/O tests. Native-platform source acceptance is tracked separately; this Wine
result is also separate from the earlier fixed DNS misalignment issue.

Before claiming Wine compatibility, rerun the public and live checks under
the intended target Wine release or a newer controlled Wine environment and
compare the platform-trust behavior. Do not add a verification bypass,
plaintext fallback, `catch_unwind` workaround, or mutation of system trust or
the shared private prefix based on these observations.

## GDS policy trace

Read-only history inspection found that the HTTP bypass predates the documented Wine backend switch. Commit `ad32c03e` (2025-08-21) introduced reqwest's `danger_accept_invalid_certs(true)` without a recorded rationale. Commit `58dcdc62` (2026-07-09) switched Rust HTTP to ureq for Wine and retained the bypass; `7642d481` (2026-08-17) carried that policy into the NBReq adapter. Both current HTTP adapters disable verification unconditionally and offer no per-request verification choice. Wine may have helped perpetuate the policy, but this history does not prove the original reason.

Rust DPGPI is separate: verification defaults to true, and an explicit false setting persists on the instance until reset; no current Rust caller was found setting it. The inspected Delphi GPI SSL setup has no explicit disable flag. The focused source/history record is retained as `gds-policy-history.md` in the new evidence lab. No GDS policy was changed.
