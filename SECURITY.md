# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's **Report a
vulnerability** link on the repository's Security page; private vulnerability reporting is enabled.

Include the affected NBReq version or commit, target platform, a minimal reproduction, and the
expected impact. Say whether any credentials or other sensitive data may have been exposed, but do
not include production secrets, private certificates, customer payloads, or other sensitive data in
the initial report. We will acknowledge the report and coordinate investigation and disclosure as
capacity permits; this pre-1.0 project does not yet promise a fixed response-time SLA.

If private reporting is temporarily unavailable, retain the report and contact the maintainer
through a private channel rather than publishing the details.

## Supported versions

Only the latest published 0.x release is supported. Users of older 0.x releases may be asked to
upgrade before receiving a fix. This policy will be revisited before 1.0.

## Security posture

NBReq verifies TLS certificate chains and DNS/IP identities. Platform trust is the default for
HTTPS, direct TLS and STARTTLS, even when the optional `bundled-roots` feature is enabled.
`TlsTrust::SuppliedRootsOnly` uses only the Engine's supplied DER roots;
`TlsTrust::BundledMozilla` requires that feature and uses compiled Mozilla roots plus any supplied
roots. Both are explicit WebPKI policies: they retain certificate signatures, validity and identity
checks but do not inherit OS enterprise roots, distrust rules or revocation retrieval, and add no
online revocation checking. Choose separate Engines for separate trust domains.

Trust selection never disables verification or falls back to another policy. Malformed roots,
empty supplied-only trust and unsupported configurations fail construction. In 0.2.1, additional
roots pass WebPKI trust-anchor parsing before platform verification too, so OS acceptance differences
can surface as construction failures. Public `test-support` backends also reject trust settings
they cannot apply with `Unsupported`. Bundled roots change only after dependency updates, a rebuild
and deployment; running applications do not download replacements. See
[TLS trust configuration](docs/tcp-tls.md#selecting-certificate-trust).

The legacy HTTP-request `DangerouslyDisableCertificateVerification` option is a compatibility
escape hatch and should not be used in ordinary deployments. Standalone TLS has no such option,
and a failed consuming upgrade closes the original connection without plaintext fallback.
Resource limits, cancellation, and consuming Engine shutdown are
part of the public contract. NBReq's public diagnostics are intended to be payload-free, but callers
remain responsible for protecting request and response values they choose to log.

NBReq proper forbids unsafe Rust. Windows polling/DNS discovery and macOS System Configuration
conversion/notification FFI are isolated in the implementation-detail `nbreq-winpoll` and
`nbreq-darwin` support crates behind safe interfaces. Neither helper is intended as a standalone
consumer API. macOS resolver configurations that cannot be represented safely are rejected
rather than flattened into an incorrect global DNS route.

Portable TLS tests and unauthenticated live transport probes passed for Win32 binaries on Wine
5.0 (Ubuntu package 5.0-3ubuntu1) with a private Win32 prefix and an app-local ProcessPrng shim.
Platform trust still failed in that environment. This scoped evidence is not a general Wine or
SMTP delivery claim; validate the deployed trust policy and environment.

Windows DNS discovery reads only the needed IP Helper fields through bounds-checked byte slices.
It does not decode unused adapter descriptions or friendly names; malformed required records
return errors. DNS ranking, interface filtering and registry search-suffix policy remain in NBReq.

## Dependency updates

Runtime dependency requirements normally accept Cargo-compatible releases. This lets consuming
applications resolve a shared graph and receive compatible security fixes without waiting for an
NBReq manifest change. Applications retain control through their own lockfiles and update process.
NBReq's lockfile and release evidence identify the graph we tested; they do not pin the transitive
graph of a downstream application. Any future exact runtime pin needs a documented compatibility
reason. Test/tool pins, including the exception below, do not impose those development dependencies
on applications that use NBReq as a library.

NBReq 0.2 requires rustls 0.23.45 or a compatible newer release. This minimum excludes the
TLS 1.3 encryption-level validation issue in
[RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html), reported September 14,
2026. The advisory applies to earlier allowed/locked rustls versions; it is not an exception.

## Reviewed advisory exceptions

The test graph pins `time` 0.3.45 through the `rcgen` certificate-fixture generator.
`RUSTSEC-2026-0009` affects only RFC-2822 parsing functions. NBReq compiles neither `time`'s
parsing feature nor any RFC-2822 parser; tests use it only to construct certificate validity dates.
The newer 0.3.46 and fixed 0.3.47 releases require Rust 1.88. This dev-only exception preserves
NBReq's Rust 1.85 MSRV and does not enter the library's runtime dependency graph. It must be
reassessed if fixture generation starts parsing untrusted time values or the test-certificate
machinery changes.

NBReq owns its bounded DNS question encoder and response decoder. The runtime dependency graph does
not include a general DNS message implementation or DNSSEC stack. Parser bounds, retained packet
seeds, and adversarial campaigns are part of the release gate; changes to that private wire codec
must preserve those checks.
