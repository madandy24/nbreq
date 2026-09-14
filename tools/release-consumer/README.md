# Independent release consumer

Unpublished verification tooling. `run.py` copies these tests into an independent temporary
Cargo workspace and unpacks the supplied normalized `.crate` files there. It never uses GDS,
imports private nbreq modules, changes host networking or publishes anything.

Pass `--package PATH/nbreq-0.2.0.crate --darwin-package PATH/nbreq-darwin-0.1.0.crate
--out NEW_EVIDENCE_DIRECTORY`, optionally `--offline` and `--toolchains stable 1.85.0`.
The output directory must be new. It preserves source, dependency locks, commands and logs;
the temporary workspace is retained for investigation. Remove it only after checking its exact
path in `inputs.json` and preserving required evidence.

The Darwin override is explicit because that support crate is not yet published. Passing these
checks cannot close the registry-only release gate. Offline resolution uses the cached index;
it must not be described as a fresh online dependency check.

Default/native-only tests use ordinary Engines and bounded loopback fixtures. Only the separately
selected test-support case injects a DNS server for a real UDP query. Import probes verify that
Resolver and testing capabilities disappear without their features. Common HTTP tests run
unchanged against actual registry nbreq 0.1.1 and the candidate package. Every fixture bounds
accept/read/write waits and joins its worker. Cargo commands have a ten-minute outer bound.

The runner also builds all seven examples from the unpacked root archive and runs the manual,
spawned, bounded HTTP, FFI owner and TCP echo examples against local fixtures where needed.
Pass `--live-dns example.com --live-https https://example.com/` to additionally exercise the
packaged Resolver and platform-trusted HTTPS examples using the host's ordinary configuration.
These optional checks require working external networking and do not change host settings.
Example processes have 45-second bounds (60 seconds for live HTTPS).

The root package allowlist excludes this directory. Use `cargo test --lib` here; the probe bins
deliberately fail in feature combinations where their imported surface should be absent.

## Pre-R5 dependency and packaging checks

`python pre_r5.py --out NEW_EVIDENCE_DIRECTORY` resolves fresh online consumer graphs on stable
and Rust 1.85, both alone and alongside an explicit newer compatible Mio requirement. It runs
default/native-only/minimal/test-support tests and negative feature probes, preserving locks,
compiler versions and logs. It uses local root, Darwin and winpoll overrides, so these checks
do not establish registry-only installation.

`python package_candidate.py --out NEW_EVIDENCE_DIRECTORY` requires a clean candidate checkout
and packages all three crates with locked offline verification into that fresh directory. Root
verification uses explicit current local helper overrides. `packages.json` names each exact
archive, source commit, SHA256, included file and relative documentation target. It neither
selects old archives from another target directory nor uploads/publishes anything. Native macOS
behavior still requires macOS execution; compiling its gated support crate on Windows is not
macOS validation. Both scripts require Python 3.11 or later and bound each Cargo command to
15 minutes.
