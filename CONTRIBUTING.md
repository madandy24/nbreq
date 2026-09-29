# Developing NBReq

NBReq requires Rust 1.85 or later and uses Rust 2024 edition. The tested native targets are
Windows x64, Linux x64 with an Ubuntu 20.04 ABI baseline, and Intel/Apple Silicon macOS.
Focused Win32/Wine evidence has additional environment and trust-policy requirements; see
[platform scope](docs/getting-started.md#platform-scope) before making deployment claims.

The settled project and crate name is NBReq / `nbreq` (Non-Blocking Request). Copyright is held by
Cave Rock Software Limited and the public grant is `MIT OR Apache-2.0`. The public repository is
`https://github.com/madandy24/nbreq`. The published implementation-detail support crates are
`nbreq-winpoll 0.1.1` and `nbreq-darwin 0.1.0`; core releases remain explicit, reviewed maintainer
actions. The separate `nbreq-smtp 0.1.0` workspace crate remains unpublished and requires core
NBReq 0.3.0 or newer within the compatible line.

Unless explicitly stated otherwise, any contribution intentionally submitted for inclusion in
NBReq is licensed under the same `MIT OR Apache-2.0` terms, without additional conditions.

## Required checks

Run the cross-platform verification entry point before committing:

```text
cargo run --manifest-path tools/xtask/Cargo.toml -- verify
```

It first checks its own formatting, tests, and warning-denied lint, then checks the private WinSock
compatibility wrapper before printing and running the frozen NBReq formatting, compilation,
warning-denied lint, default/minimal/native/all-feature test, doctest, documentation, and named
pressure-regression gates, plus SMTP formatting, lint, tests, doctests, docs and example builds.
It flushes each exact command before execution, reports elapsed time per
stage, and stops at the first failure. Use
`--offline` on an exact-source host with a populated Cargo cache, and
`--stress-repetitions 25` when repeating the pressure gate. `--dry-run` prints the complete command
plan without executing it.

The entry point currently expands to these principal commands (plus the named pressure filters):

```text
cargo fmt --check
cargo fmt --manifest-path support/winpoll/Cargo.toml --check
cargo check --manifest-path support/winpoll/Cargo.toml --all-targets
cargo clippy --manifest-path support/winpoll/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path support/winpoll/Cargo.toml
cargo check --no-default-features
cargo clippy --no-default-features --all-targets -- -D warnings
cargo check --no-default-features --features native --all-targets
cargo clippy --no-default-features --features native,test-support --all-targets -- -D warnings
cargo test --no-default-features --features native,test-support
cargo check --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
cargo test
cargo test --no-default-features
cargo test --features native,test-support
cargo test --all-features
cargo test --all-features --doc
cargo doc --all-features --no-deps
cargo fmt --package nbreq-smtp --check
cargo clippy --package nbreq-smtp --all-targets -- -D warnings
cargo test --package nbreq-smtp --all-targets
cargo test --package nbreq-smtp --doc
cargo doc --package nbreq-smtp --no-deps
cargo build --package nbreq-smtp --examples
```

The crate enables Rust's `missing_docs` lint. The existing warning-denied all-feature lint stage
therefore also prevents undocumented public API from entering the release surface.

The public-repository CI runs that complete verifier on stable Rust and Rust 1.85 for Windows,
Ubuntu, macOS 15 Intel and macOS 15 Apple Silicon. Each job fetches the exact lock graph first and
then executes the verifier offline, builds and runs local examples, and checks independent
consumer dependency graphs. Current stable uses fresh resolution; Rust 1.85 explicitly selects
`yoke-derive 0.8.2` in the consumer lockfile for the documented upstream compiler issue,
preserving the initial lock and selection record. No library-wide dependency pin is added.
See [Rust version and dependency selection](docs/getting-started.md#rust-version-and-dependency-selection).
The separate manual registry workflow covers those same platform/toolchain
combinations for a candidate archive or published package. A separate stable-Ubuntu job runs
`cargo-audit 0.22.2`; the reviewed
exception in `.cargo/audit.toml` is justified in `SECURITY.md` and must not be expanded without a
source-level reachability review.

The checked-in component and dependency license report is generated with pinned `cargo-about
0.9.1`. After installing that tool, refresh and verify the report with:

```text
cargo about generate --frozen --workspace --all-features --fail --output-file THIRD_PARTY_LICENSES.html about.hbs
```

CI regenerates the report independently and rejects drift from the exact locked graph or accepted
license policy in `about.toml`.

The accepted pre-release curl pilot is no longer part of the public manifest or ordinary verifier.
Its source, tests, and Windows proof scripts remain historical/reference material. To reproduce the
accepted pilot, check out commit `b60dbe0` (or an earlier named evidence commit) before running:

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/test-curl-windows.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File tools/test-curl-dll-windows.ps1
```

These specialized scripts are not hidden inside the cross-platform entry point. They use the
installed Visual Studio C++ tools, download and hash-check the pinned official
curl source, and place all generated artifacts under `target/curl-pilot`. They do not require a
global curl or vcpkg installation.

Backend implementation types stay private. Public request, response, lifecycle, cancellation, and
error types must compile with the ordinary native feature set and with no default features.

New dependencies require a recorded reason, supported-platform review, license review, and confirmation that they do not introduce an async runtime or leak backend-specific types into the public API.
