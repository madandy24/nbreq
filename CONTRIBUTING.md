# Developing NBReq

NBReq currently requires Rust 1.85 or later and uses Rust 2024 edition. The initial supported deployment targets are Windows 10 x64 or later, the Windows build under Ubuntu 20.04's default Wine, and native Linux x64 built against an Ubuntu 20.04 ABI baseline.

The settled project and proposed crate name is NBReq / `nbreq` (Non-Blocking Request). The intended
public grant is `MIT OR Apache-2.0`; the crate remains private and `publish = false` until the
displayed copyright holder, publication metadata, and WP10/WP11 release gates are complete.

## Required checks

Run these before committing:

```text
cargo fmt --check
cargo check --no-default-features
cargo check --all-features --all-targets
cargo clippy --all-features --all-targets -- -D warnings
cargo test --no-default-features
cargo test --all-features
```

On Windows, the exact dynamic curl pilot build and DLL lifecycle proof are run with:

```text
powershell -NoProfile -ExecutionPolicy Bypass -File tools/test-curl-windows.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File tools/test-curl-dll-windows.ps1
```

These scripts use the installed Visual Studio C++ tools, download and hash-check the pinned official
curl source, and place all generated artifacts under `target/curl-pilot`. They do not require a
global curl or vcpkg installation.

Backend implementation types stay private. Public request, response, lifecycle, cancellation, and error types must compile identically with no backend feature, private `curl-pilot`, `native`, and both backend features enabled.

New dependencies require a recorded reason, supported-platform review, license review, and confirmation that they do not introduce an async runtime or leak backend-specific types into the public API.
