# GDS portable verified TLS integration handoff

Prepared from read-only GDS source on 2026-09-26. No GDS files, configuration,
deployment or running service were changed. NBReq production implementation is at
`b5c8eb0`; test-only fixture repair follows at `8d24e27`. Native Windows and both Macs,
Rust 1.85 and package consumers have passed the qualified checks in the
[portable TLS plan](nbreq_portable_tls_plan.md). Actual Wine execution remains pending
Linux upload approval. Attach accepted Wine results before relying on this remedy there.

## Integration points

- [GDS Cargo.toml](C:/User/SecuritasNew/gds/rust/gds/Cargo.toml:13) forwards
  `nbreq-native` to `nbreq/native`; line 36 pins NBReq `=0.2.0`. The new trust API is
  unreleased and absent from that published version. First adopt the approved NBReq
  release/revision, then enable `nbreq/bundled-roots` in the intended GDS build feature.
- [Engine construction](C:/User/SecuritasNew/gds/rust/gds/src/dplib/dphttpclient.rs:433)
  uses `Engine::builder()` and existing memory/concurrency limits. Add
  `.config(nbreq::EngineConfig::spawned().with_tls_trust(nbreq::TlsTrust::BundledMozilla))`
  **before** the existing limit setters: `config` replaces the builder configuration.
  Add approved private CA DERs with `with_additional_tls_root_certificate` if required.
  Platform remains NBReq's default even when the bundle feature is compiled.
- [Request construction](C:/User/SecuritasNew/gds/rust/gds/src/dplib/dphttpclient.rs:1094)
  unconditionally selects `DangerouslyDisableCertificateVerification`. Remove this
  override or explicitly select `TlsVerification::Verify` for the verified path.
  Selecting bundled roots alone does not override an explicitly insecure request.
  Update the existing [insecure-policy assertion](C:/User/SecuritasNew/gds/rust/gds/src/dplib/dphttpclient.rs:1709)
  to prove verified requests and retain the finite timeout assertions.

The legacy [ureq agent](C:/User/SecuritasNew/gds/rust/gds/src/dplib/dphttpclient.rs:1099)
also uses a certificate/signature bypass. Changing NBReq does not secure that backend.
Whether to remove or retain any intentionally insecure compatibility path is a separate
owner decision; it must never be an automatic retry after verification failure. Identify
the selected backend and policy in integration evidence without logging credentials.

## Policy decisions and acceptance

BundledMozilla uses WebPKI to verify signatures, validity and server identity against
Mozilla plus supplied roots. It does not inherit OS enterprise trust/distrust or platform
revocation retrieval. Confirm this policy fits GDS deployments, identify authorized private
CAs, and choose whether separately configured platform-trust Engines are required. Use
separate Engines for distinct trust domains; roots are immutable per Engine.

Before deployment, prove:

1. **Startup:** the intended feature/policy is selected, malformed DER and unavailable
   configuration fail clearly, and current limits remain applied. Exercise the existing
   [service creation path](C:/User/SecuritasNew/gds/rust/gds/src/dplib/dpsyscontext.rs:521).
2. **HTTPS and remote scans:** valid public/private destinations succeed as configured;
   wrong-name, expired and untrusted peers fail with TLS errors, including redirects and
   retries. Run the actual remote-scan fetch/sync workflow, including
   [sync_activity_to_local](C:/User/SecuritasNew/gds/rust/gds/src/app/dsextclient_gds.rs:1343),
   and check unchanged admission, cancellation, deduplication and error handling.
3. **Shutdown:** during an active verified request/scan, close admission, cancel work and
   join the Engine through [DpSysContext::shutdown](C:/User/SecuritasNew/gds/rust/gds/src/dplib/dpsyscontext.rs:487).
   Existing client clones must stop accepting work; no socket/thread leaks or hang.
   Repeat on each supported native platform and the actual Wine deployments.

## Root maintenance

NBReq uses a compatible `webpki-roots` 1.x range (minimum 1.0.9), so compatible root
updates need not wait for an NBReq release. Assign root-bundle review to each GDS
release and an expedited security-update path. Update the appropriate 1.x package in
GDS's lockfile, review the change, rerun trust regressions, rebuild and redeploy; running
binaries do not refresh roots. GDS already declares a separate `webpki-roots` 0.26.x
dependency: updating that older package does not update NBReq's 1.x bundle. Record the
resolved bundle version and lockfile/binary identity in release evidence.
