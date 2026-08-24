# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Once the public repository exists,
use GitHub's **Report a vulnerability** link on the repository's Security page. Private
vulnerability reporting must be enabled before the first release.

Include the affected NBReq version or commit, target platform, a minimal reproduction, and the
expected impact. Say whether any credentials or other sensitive data may have been exposed, but do
not include production secrets, private certificates, customer payloads, or other sensitive data in
the initial report. We will acknowledge the report and coordinate investigation and disclosure as
capacity permits; this pre-1.0 project does not yet promise a fixed response-time SLA.

If private reporting is temporarily unavailable before repository setup is complete, retain the
report and contact the maintainer through a private channel rather than publishing the details.

## Supported versions

Before the first crates.io release, only the current repository `main` is reviewed for security
fixes. After 0.1.0, the latest published 0.x release is supported; older 0.x releases may be asked to
upgrade before receiving a fix. This policy will be revisited before 1.0.

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

The Rust-1.85 release graph currently pins `hickory-proto` 0.25.2 for DNS wire types. Two RustSec
advisories match that package version but their affected paths are not reachable through NBReq:

- `RUSTSEC-2026-0118` affects `DnssecDnsHandle` when a DNSSEC feature is enabled. NBReq disables all
  Hickory DNSSEC features and implements its own resolver owner.
- `RUSTSEC-2026-0119` affects encoding attacker-shaped messages containing many records. NBReq's
  production encoder constructs exactly one bounded A or AAAA question and zero records; untrusted
  DNS responses are decoded, never re-encoded. A regression test locks down that encoder shape.

The fixed Hickory 0.26.1 release requires Rust 1.88 and therefore cannot replace this pin while
NBReq promises Rust 1.85. Removing the wire-only dependency is scheduled as early follow-up work.
The advisory exceptions must be reassessed whenever Hickory usage, features, MSRV, or DNS encoding
changes.
