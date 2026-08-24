# WP11.3 release-automation evidence

Status: local checkpoint prepared 2026-08-24. This record does not claim that the public GitHub
repository, hosted CI, private vulnerability reporting, or crates.io publication exists yet.

## 1. Portable CI definition

`.github/workflows/ci.yml` is prepared for the genuine public repository with read-only contents
permission and an immutable full-length `actions/checkout` v6.0.2 commit pin. It runs the existing
complete verifier on:

- current stable Rust on Ubuntu;
- current stable Rust on Windows; and
- the declared Rust 1.85.0 MSRV on Ubuntu.

Each verification job fetches the exact locked graph and then runs the verifier offline. Separate
Ubuntu jobs run pinned `cargo-audit 0.22.2` and regenerate the dependency-license report with pinned
`cargo-about 0.9.1`. These portable jobs supplement rather than replace the accepted Ubuntu 20.04,
Windows 10, Wine 5, GDS x86, DLL-load, and live-canary records.

## 2. Advisory policy

`.cargo/audit.toml` enables yanked-package checking and records exactly three reviewed exceptions:

- `RUSTSEC-2026-0009`, confined to dev-only certificate fixture construction without the affected
  `time` parsing feature;
- `RUSTSEC-2026-0118`, whose Hickory DNSSEC path is not compiled; and
- `RUSTSEC-2026-0119`, whose affected general encoder is outside NBReq's regression-locked one
  bounded-question, zero-record production shape.

The matching source-level rationales remain in `SECURITY.md` and `wp11_security_api_audit.md`.
Eliminating the wire-only Hickory dependency remains early post-WP11 work rather than a permanent
exception policy. A local offline scan of the 139-package lock graph accepts only these reviewed
exceptions and exits successfully.

## 3. Exact component and dependency licenses

`about.toml` freezes the accepted permissive license set and the Windows x64/Linux x64 release
targets. `about.hbs` produces `THIRD_PARTY_LICENSES.html` from the locked non-development graph with
all release features enabled. The report contains the two workspace components and their release
dependencies plus the complete selected license texts.

Two independent local generations produced the identical SHA-256:

`C35AEB2FE51B4F6EBDD9C3B27E70C8DEE68BF8FE2A0608582D187429F1340A30`

The report is included explicitly in the root crate package and marked generated for repository
language statistics. CI performs the same generation and rejects byte-level drift.

## 4. Local verification and package boundary

The complete Windows verifier passes all 20 stages in 29.940 seconds after the automation and
notice files are present. `git diff --check` is clean. `cargo package --list --allow-dirty` includes
the generated report and continues to exclude planning records, proving tools, archived pilot
materials, and other non-product files.

## 5. Remaining WP11.3 gates

1. Create `https://github.com/madandy24/nbreq` without generated starter files and push the exact
   reviewed local history.
2. Observe the Windows, Ubuntu, MSRV, RustSec, and license-report jobs succeeding on GitHub.
3. Enable GitHub private vulnerability reporting and verify the packaged `SECURITY.md` route.
4. Rehearse registry resolution by publishing and verifying `nbreq-winpoll 0.1.0` before packaging
   the root crate. Publication itself requires a separate explicit release decision.
5. Build the root archive and a clean external consumer only after the support crate resolves from
   the registry.

No crate has been published by this checkpoint.
