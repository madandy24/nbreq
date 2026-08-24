# Developing NBReq

NBReq currently requires Rust 1.85 or later and uses Rust 2024 edition. The initial supported deployment targets are Windows 10 x64 or later, the Windows build under Ubuntu 20.04's default Wine, and native Linux x64 built against an Ubuntu 20.04 ABI baseline.

The settled project and proposed crate name is NBReq / `nbreq` (Non-Blocking Request). Copyright is
held by Cave Rock Software Limited and the public grant is `MIT OR Apache-2.0`. The repository is
intended for `https://github.com/madandy24/nbreq`; the crate remains private and `publish = false`
until support-crate publication and the remaining WP11 release gates are complete.
WP10's native-default and platform gates are accepted.

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
pressure-regression gates. It flushes each exact command before execution, reports elapsed time per
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
cargo check --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
cargo test
cargo test --no-default-features
cargo test --features native,test-support
cargo test --all-features
cargo test --all-features --doc
cargo doc --all-features --no-deps
```

The crate enables Rust's `missing_docs` lint. The existing warning-denied all-feature lint stage
therefore also prevents undocumented public API from entering the release surface.

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
