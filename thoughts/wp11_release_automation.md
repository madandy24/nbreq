# WP11.3 release-automation evidence

Status: WP11.3 accepted 2026-08-24. The public GitHub repository, portable CI, private vulnerability
reporting, support-crate publication, registry-resolution rehearsal, and clean consumer build pass.
The root `nbreq` crate remains unpublished pending the separate WP11.4 decision.

## 1. Portable CI definition

`.github/workflows/ci.yml` runs on the genuine public repository with read-only contents
permission and an immutable full-length `actions/checkout` v6.0.2 commit pin. It runs the existing
complete verifier on:

- current stable Rust on Ubuntu;
- current stable Rust on Windows; and
- the declared Rust 1.85.0 MSRV on Ubuntu.

Each verification job fetches the exact locked graph and then runs the verifier offline. Separate
Ubuntu jobs run pinned `cargo-audit 0.22.2` and regenerate the dependency-license report with pinned
`cargo-about 0.9.1`. These portable jobs supplement rather than replace the accepted Ubuntu 20.04,
Windows 10, Wine 5, GDS x86, DLL-load, and live-canary records.

GitHub Actions run [#3](https://github.com/madandy24/nbreq/actions/runs/32680496609) on commit
`9015961` completes successfully in 3 minutes 2 seconds. The matrix passes stable Ubuntu, stable
Windows, and Ubuntu/Rust 1.85; the RustSec and byte-exact license jobs pass separately. The first
fresh runner usefully exposed and closed two workflow-only assumptions: the frozen license job now
fetches the lock graph before generation, and its temporary output no longer assumes another job
has created `target/`.

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

## 5. Public repository controls

The empty public repository was created at `https://github.com/madandy24/nbreq`, then populated by
pushing the reviewed local `main` history. GitHub Actions is enabled. Private vulnerability
reporting is enabled, making the packaged `SECURITY.md` route live. Secret scanning and push
protection remain enabled by GitHub for the public repository.

## 6. Registry-resolution and consumer rehearsal

With explicit owner approval, `nbreq-winpoll 0.1.0` was published from clean commit `0ac593b` after
an authenticated dry run. Its eight-file archive is 7.6 KiB compressed with SHA-256:

`A7D9DF03F084102285D0AED63DE4233544FC23D9D555D0E4A0DB5E30A27BDEB1`

The crates.io record reports `MIT OR Apache-2.0`, Rust 1.85, and the expected repository and docs.rs
URLs. Root `cargo package` then succeeds without a local registry patch, explicitly downloads and
compiles `nbreq-winpoll 0.1.0`, and produces the 39-file `nbreq 0.1.0` archive with SHA-256:

`5689F12B3E560DD477F62E978D025CAEAD0C0363F6914714C646AF6E653DE9D2`

A new isolated consumer depends on the unpacked normalized root package, resolves the support crate
from crates.io, compiles the complete native graph, creates one spawned Engine and Client, and
shuts the Engine down successfully. This closes WP11.3 without publishing the root crate.

## 7. WP11.4 root release candidate

The root manifest now permits publication only to `crates-io`; it does not make publication an
automatic CI action. The first exact archive was held rather than published after review found
pre-release path-dependency, optional-curl, future-platform-matrix, and security-support wording in
the packaged guide/rustdoc. The replacement makes those consumer instructions durable for 0.1.0.
Before the explicit, permanent publication of `nbreq 0.1.0`, freeze the corrected clean commit,
rebuild and consume its normalized archive, require green hosted CI, and present the commit and
archive hash for owner approval.
