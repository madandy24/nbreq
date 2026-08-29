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

NBReq 0.1.0 is the initial supported release. As later 0.x versions are published, only the latest
published 0.x release is supported; users of older 0.x releases may be asked to upgrade before
receiving a fix. This policy will be revisited before 1.0.

## Security posture

NBReq verifies TLS certificate chains and hostnames by default. The deliberately verbose
`DangerouslyDisableCertificateVerification` option is a compatibility escape hatch and should not
be used in ordinary deployments. Resource limits, cancellation, and consuming Engine shutdown are
part of the public contract. NBReq's public diagnostics are intended to be payload-free, but callers
remain responsible for protecting request and response values they choose to log.

NBReq proper forbids unsafe Rust. The small Windows compatibility FFI boundary is isolated in the
published implementation-detail `nbreq-winpoll` support crate and exposed to NBReq through a safe
interface.

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
