# Independent release consumer

Unpublished verification tooling. `run.py` copies these tests into an independent temporary
Cargo workspace and unpacks the supplied normalized `.crate` files there. It never uses GDS,
imports private nbreq modules, changes host networking or publishes anything.

Pass `--package PATH/nbreq-0.3.0.crate --darwin-package PATH/nbreq-darwin-0.1.0.crate
--winpoll-package PATH/nbreq-winpoll-0.1.1.crate
--out NEW_EVIDENCE_DIRECTORY`, optionally `--offline` and `--toolchains stable 1.85.0`.
The output directory must be new. It preserves source, dependency locks, commands and logs;
the temporary workspace is retained for investigation. Remove it only after checking its exact
path in `inputs.json` and preserving required evidence.

The Darwin/winpoll overrides let this runner check supplied support candidates independently
of their published versions. Passing these checks cannot close the registry-only release gate;
use `registry.py` below for that evidence. Offline resolution uses the cached index;
it must not be described as a fresh online dependency check.

## Consumer lock policy

All three runners inspect the selected compiler's actual `rustc -vV` output. Rust 1.87 and newer
use the unmodified fresh consumer graph. Older compilers, including Rust 1.85, use an explicit
**compatibility-selected** graph: after resolution the runner executes
`cargo update -p yoke-derive --precise 0.8.2` in that consumer workspace. Version 0.8.3 uses an API
unavailable before Rust 1.87 without declaring that requirement. This policy changes consumer
lockfiles only; it does not constrain NBReq's published dependency requirements.

Each case retains `Cargo.initial.lock`, the selected `Cargo.lock`, and `consumer-policy.json`
with effective compiler identity, route, exact command, hashes and package changes. Selection
is explicit before compilation, never a retry after an arbitrary build failure. Unexpected
unrelated package changes fail the check. The real 0.1.1 comparison in `run.py` uses the same
policy in separate workspaces. Toolchains never share a mutable consumer lock.

Packaged examples use the archive's existing lock with `--locked`, including on Rust 1.85;
its copied lock and hash are retained separately. They are not fresh-graph proof. Run the
policy regression checks with `python tools/release-consumer/test_consumer_policy.py`.

Default/native-only tests use ordinary Engines and bounded loopback fixtures. Only the separately
selected test-support case injects a DNS server for a real UDP query. Import probes verify that
Resolver and testing capabilities disappear without their features. Common HTTP tests run
unchanged against actual registry nbreq 0.1.1 and the candidate package. Every fixture bounds
accept/read/write waits and joins its worker. Cargo commands have a ten-minute outer bound.

The additional `bundled-roots` mode forwards the optional NBReq feature, checks that the default
trust policy remains `Platform`, and constructs and shuts down an Engine explicitly selecting
`BundledMozilla`. Bundle-disabled native modes require that selection to return `Unsupported`.
Metadata verifies the enabled NBReq feature and its active registry `webpki-roots` dependency.
This is construction and dependency-graph coverage; the local C04/C05 fixtures exercise TLS using
supplied roots. The unchanged HTTP comparison still consumes actual registry NBReq 0.1.1.

The runner builds all 19 numbered examples from the unpacked root archive and uses
`check_examples.py` to run all 11 HTTP and five TCP programs against local fixtures, including
direct TLS and a TCP-to-TLS upgrade. Two additional cases check that HTTP 404 is a response and
an oversized declared body fails the configured limit, for 18 local executions in total.
Cancellation must produce a verified terminal `Cancelled`; echo checks require exact bytes and EOF.
Pass `--live-dns example.com --live-https https://example.com/` to additionally exercise the
three packaged DNS programs and the simple HTTPS GET using the host's ordinary configuration.
These optional checks require working external networking and do not change host settings.
Example processes have 45-second bounds. The local-only CI command is:

```sh
cargo build --locked --offline --examples
python tools/release-consumer/check_examples.py --bin-dir target/debug/examples --out target/example-results
```

The result directory must be new. Binary hashes, individual logs and a JSON summary are retained;
DNS execution is explicitly skipped unless requested. This runner requires Python 3.11 or newer.

The root package allowlist excludes this directory. Use `cargo test --lib` here; the probe bins
deliberately fail in feature combinations where their imported surface should be absent.

## Pre-R5 dependency and packaging checks

`python pre_r5.py --out NEW_EVIDENCE_DIRECTORY` resolves online consumer graphs on stable
and Rust 1.85, both alone and alongside an explicit newer compatible Mio requirement, then applies
the compiler-specific lock policy above. It runs
default/native-only/minimal/test-support/bundled-roots tests and negative feature probes, preserving locks,
compiler versions and logs. It uses local root, Darwin and winpoll overrides, so these checks
do not establish registry-only installation.
Pass `--toolchains stable` or `--toolchains 1.85.0` to select the installed toolchain in a CI
matrix job. Inputs include the source commit, host platform and selected toolchains.

`python package_candidate.py --out NEW_EVIDENCE_DIRECTORY` requires a clean candidate checkout
and packages all three crates with locked offline verification into that fresh directory. Root
verification uses explicit current local helper overrides. `packages.json` names each exact
archive, source commit, SHA256, included file and relative documentation target. It neither
selects old archives from another target directory nor uploads/publishes anything. Native macOS
behavior still requires macOS execution; compiling its gated support crate on Windows is not
macOS validation. Both scripts require Python 3.11 or later and bound each Cargo command to
15 minutes.

## Registry release checks

`registry.py --mode candidate --package EXACT_ROOT_ARCHIVE --out NEW_DIRECTORY` tests a
normalized root candidate with Darwin/winpoll resolved only from crates.io. Its sole local
override is the unpublished root package. `--mode published --out NEW_DIRECTORY` downloads
nbreq 0.3.0 and tests it with no local overrides. Both modes check Cargo metadata and lock
checksums, fresh current-compiler and compatibility-selected MSRV consumers with and without
Mio coexistence, feature boundaries,
the optional Mozilla-root graph and all 18 local example cases. The bundled-roots mode runs on
each selected toolchain in both candidate and published checks. `--toolchains` selects installed
toolchains for a matrix job.

The manual `Registry release checks` workflow runs these modes on Windows, Linux and both
Mac architectures, stable and Rust 1.85. It has read-only repository permission and never
publishes packages. Candidate mode proves registry support dependencies; only published mode
establishes a fully registry-resolved NBReq consumer. Exact package hashes and source identity
remain in the evidence records; package/source build times are not performance measurements.
