# DNS refresh correction: 0.2.2 and 0.3.1

Opened 22 October 2026. The owner requested the fix on both release lines. GDS will
verify 0.2.2 before migrating to 0.3.1; its rebuild, restart and live CAT2 checks
belong to the originating GDS session.

Completed: both releases are published, tagged, and verified from their exact
registry archives on Windows, Linux, Intel macOS and Apple Silicon, on stable
Rust and Rust 1.85. The [release handoff](nbreq_dns_refresh_release_handoff.md)
records final identities, raw evidence, exceptions and the GDS update sequence.

## Scope and baselines

- Current line: 9c3723f (published 0.3.0 source at ae7bfeed), target 0.3.1.
- Maintenance line: published v0.2.1 / 44bbd1b, target 0.2.2.
- DNS implementation and tests are identical on both starting points.
- Preserve each line's timeout defaults and public API/error classifications.
- No helper release, IPv4 filtering or OS network change.

## Fix and evidence

Refresh uses startup's ordered socket selection and keeps the actual index, including
for TCP DNS fallback. Unavailable-state diagnostics retain discovery, socket or
registration errors; successful recovery clears the saved error. Existing generation,
pending-query completion, cache, idle HTTP eviction and shutdown behavior is retained.

Private tests inject socket failures through the existing refresh configuration source.
The original code failed both new regressions (index zero and generic error); raw proof
is in `target/dns-refresh-20261022/regression-red.log`. After the fix, all 52 then-current
native-only DNS tests passed. A further TCP fallback regression and discovery diagnostic
assertions are included in the final validation.

## Release gates

Completed: full local verifier on both lines; Windows x86 native/default DNS checks;
clean package identities and dry runs; hosted stable/MSRV verification and registry
candidate consumers on Windows, Linux, Intel macOS and ARM macOS; advisory/license
checks; publication, exact registry archive verification and published consumers.

The registry runner now executes DNS tests from the exact packaged source in both
feature modes, plus Windows x86 tests and native-only consumer checks.

Both release matrices passed. GDS remains unchanged in this session.

Local 0.3.1 verification passed all 30 stages in 227.553 seconds on Rust 1.97.1.
The first sandboxed run stopped when rustdoc was denied temporary executable writes;
the unrestricted rerun passed, including all doctests. Both logs are retained.

## Security gate follow-up

The initial hosted audit found seven October advisories in the old TLS graph.
Both candidates raise rustls to 0.23.46, constrain the existing WebPKI component
to 0.103.16 for retained consumer locks, and raise optional roots to 1.0.10.
The full locked graph also selects webpki-root-certs 1.0.10. License reports and
full verification are repeated; no advisory exception is added. The extra direct
WebPKI constraint introduces no new component, async runtime or public API,
and retains its existing ISC license and supported-platform compatibility.

The owner permitted Apple Silicon coverage to remain explicitly pending only if
runner capacity prevented execution. GitHub-hosted ARM checks passed for both
published releases on both compilers, so that waiver was not used. Scaleway was
unavailable and was not needed.

## Final publication

Version 0.3.1 is tagged at `ca5dde96639d8bbd4d3394ebbb27e5dc3fad6f60`;
0.2.2 is tagged at `6e14ae7807b9c82dbb397c603b480c45f8fe0f29`. The latter
differs from tested candidate `cddb850` only by a changelog heading encoding fix;
the full local verifier passed again and the package-member delta was verified
before carrying forward runtime checks. The published matrix tested the final
archive on all platforms. The same cosmetic 0.3.1 changelog correction is in
main's closeout commit; the published archive and tag remain immutable.

The first 0.2.2 Windows stable CI attempt failed an existing short-timeout TLS
fixture test. Five unchanged local repetitions and the single hosted job rerun
passed. Original failure, rerun and diagnostic context are retained in the handoff.

The final closeout verifier passed all 30 stages in 170.881 seconds. Closeout
changes only the changelog encoding and release notes/evidence. All 28 distinct
selected dependency locks passed the final audit with no unignored vulnerabilities
or warnings. Versioned documentation for both releases is live.
