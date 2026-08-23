# Developing NBReq

NBReq currently requires Rust 1.85 or later and uses Rust 2024 edition. The initial supported deployment targets are Windows 10 x64 or later, the Windows build under Ubuntu 20.04's default Wine, and native Linux x64 built against an Ubuntu 20.04 ABI baseline.

The settled project and proposed crate name is NBReq / `nbreq` (Non-Blocking Request). The intended
public grant is `MIT OR Apache-2.0`; the crate remains private and `publish = false` until the
displayed copyright holder, publication metadata, and WP10/WP11 release gates are complete.

## Required checks

Run the cross-platform verification entry point before committing:

```text
cargo run --manifest-path tools/xtask/Cargo.toml -- verify
```

It first checks its own formatting, tests, and warning-denied lint, then prints and runs the frozen
NBReq formatting, compilation, warning-denied lint, default/minimal/native/curl/all-feature test,
doctest, documentation, and named pressure-regression gates. It flushes each exact command before
execution, reports elapsed time per stage, and stops at the first failure. Use
`--offline` on an exact-source host with a populated Cargo cache, and
`--stress-repetitions 25` when repeating the pressure gate. `--dry-run` prints the complete command
plan without executing it.

The entry point currently expands to these principal commands (plus the named pressure filters):

```text
cargo fmt --check
cargo check --no-default-features
cargo check --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
cargo test
cargo test --no-default-features
cargo test --features native,test-support
cargo test --features curl-pilot
cargo test --all-features
cargo test --all-features --doc
cargo doc --all-features --no-deps
```

On Windows, the exact dynamic curl pilot build and DLL lifecycle proof are run with:

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/test-curl-windows.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File tools/test-curl-dll-windows.ps1
```

These specialized scripts are not hidden inside the cross-platform entry point. They use the
installed Visual Studio C++ tools, download and hash-check the pinned official
curl source, and place all generated artifacts under `target/curl-pilot`. They do not require a
global curl or vcpkg installation.

Backend implementation types stay private. Public request, response, lifecycle, cancellation, and error types must compile identically with no backend feature, private `curl-pilot`, `native`, and both backend features enabled.

New dependencies require a recorded reason, supported-platform review, license review, and confirmation that they do not introduce an async runtime or leak backend-specific types into the public API.
