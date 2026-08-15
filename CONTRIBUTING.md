# Developing NBReq

NBReq currently requires Rust 1.85 or later and uses Rust 2024 edition. The initial supported deployment targets are Windows 10 x64 or later, the Windows build under Ubuntu 20.04's default Wine, and native Linux x64 built against an Ubuntu 20.04 ABI baseline.

Until the project chooses its final name and MIT/Apache licensing grant, the crate remains private and `publish = false`.

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

Backend implementation types stay private. Public request, response, lifecycle, cancellation, and error types must compile identically with no backend feature, `curl`, `native`, and both backend features enabled.

New dependencies require a recorded reason, supported-platform review, license review, and confirmation that they do not introduce an async runtime or leak backend-specific types into the public API.
