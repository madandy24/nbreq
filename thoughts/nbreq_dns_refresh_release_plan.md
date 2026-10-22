# DNS refresh correction: 0.2.2 and 0.3.1

Opened 22 October 2026. The owner requested the fix on both release lines. GDS will
verify 0.2.2 before migrating to 0.3.1; its rebuild, restart and live CAT2 checks
belong to the originating GDS session.

## Scope and baselines

- Current line: 9c3723f (published 0.3.0 source at ae7bfeed), target 0.3.1.
- Maintenance line: published v0.2.1 / 44bbd1b, target 0.2.2.
- DNS implementation and tests are identical on both starting points.
- Preserve each line's timeout defaults and public API/error classifications.
- No helper release, dependency-range change, IPv4 filtering or OS network change.

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

Pending: full local verifier on both lines; Windows x86 native/default DNS checks;
clean package identities and dry runs; hosted stable/MSRV verification and registry
candidate consumers on Windows, Linux, Intel macOS and ARM macOS; advisory/license
checks; publication, exact registry archive verification and published consumers.

The registry runner now executes DNS tests from the exact packaged source in both
feature modes, plus Windows x86 tests and native-only consumer checks.

No version is released until its gates pass. GDS remains unchanged in this session.

Local 0.3.1 verification passed all 30 stages in 227.553 seconds on Rust 1.97.1.
The first sandboxed run stopped when rustdoc was denied temporary executable writes;
the unrestricted rerun passed, including all doctests. Both logs are retained.
