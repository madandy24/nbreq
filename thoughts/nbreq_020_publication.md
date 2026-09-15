# NBReq 0.2.0 publication

2026-09-15. The owner asked to continue after the next steps were stated as helper publication,
registry-only checks and nbreq publication. The Intel Mac observation remains recorded; the
owner does not consider it a blocker given the dedicated-host evidence. Host overload is not
asserted as its proven cause.

## Resume checkpoint

| Item | State |
| --- | --- |
| Status | **R1–R6 complete. NBReq 0.2.0 and both helpers are published and independently verified from crates.io.** |
| Source | Root: isolated clean checkout `target/worktrees/nbreq-publish-020` at **`d866179719f1cd4e2efcda7e4a533fe590ae3dd6`**. Annotated **`v0.2.0`** is pushed and resolves to this exact published source. Helper source remains `432509b8d2d198cb0139def322f2cc82cd4a76c6`. Later completion records are excluded from the packages. |
| Darwin 0.1.0 | Published 2026-09-15 08:17:12 UTC. API/index and downloaded archive verified against frozen SHA256 `31d7a69844b990331f85dc497f14d0b6a08f8ceaee402e7c6aa19919f3e3c839`. |
| Winpoll 0.1.1 | Published 2026-09-15 08:17:49 UTC. API/index and downloaded archive verified against frozen SHA256 `9e4fc64245514209beb6e3feccb4b980e1c259546a9e07543e3529e1d7053696`. |
| Root publication | Published **2026-09-15 08:43:42 UTC**. **95 files / 343,650 bytes**, SHA256 **`d165747c1d62c0fa0705d901bec454ab0cae27738b4e0342bd23286ef1bfb721`**. API/index checks and an independent download match the exact final dry-run archive; version is not yanked. |
| Registry proof | Both candidate-with-registry-helpers and fully registry-only matrices pass all eight Windows/Linux/Intel Mac/ARM Mac jobs, stable Rust 1.98.1 and Rust 1.85.0. Each matrix: 16 fresh graphs, 464 tests, 32 expected negative feature probes and 128 local example executions. Published mode uses no local overrides. Native Windows x86 adds 58 tests, four negative probes and 16 example cases on Rust 1.97.1. |
| Public docs | Actual crates.io README retains Highlights above GET, release wording and a single curl mention in the final History section. Guide/example/migration links and Engine/Resolver/TcpConnector API pages load; both helper docs.rs pages load. The shared-guide rustdoc link correction is visible. |
| Remaining | No nbreq 0.2.0 release gate remains. GDS may now use the registry release; no GDS source or installed DLL changed in this pass. Application/device memory acceptance remains under M4/MQ-05. |
| Evidence | [Publication manifest](evidence/nbreq_020_publication.json) inventories 52 retained files; local Git attributes preserve their exact bytes. Local raw directory `target/release-publication-20260915` retains archives, API/index receipts and complete logs. Cargo handled credentials normally; no credential values are included. |

## Final verification

The corrected [candidate matrix](https://github.com/madandy24/nbreq/actions/runs/34947682977)
checks source `180afd7` with published helpers. The final
[published matrix](https://github.com/madandy24/nbreq/actions/runs/34948730581) checks source
`d866179` and downloads actual registry nbreq 0.2.0. Saved artifacts were independently checked
for source/package identities, registry metadata, expected feature failures, test counts and
example exit codes. Each job builds all 17 examples and executes 16 local cases; live DNS is
not claimed by this matrix. The response-limit example's intentional exit 1 is verified.

The local x86 companion records `i686-pc-windows-msvc` and an actual PE machine value `0x14c`.
It is native Windows execution, not a new Wine run. Earlier full platform verifiers, actual
Wine 5 x86 evidence and four-hour reliability observations remain in their original reports.

Five distinct locks (two candidate consumer graphs, the final package lock and two published
consumer graphs) pass cargo-audit 0.22.2 with no reported vulnerabilities or warnings, against
RustSec `e2e640471715167f73e22eaf761f2e547adafeec`, dated 2026-09-14T18:06:06+02:00.
The existing documented test-only `time` exception, RUSTSEC-2026-0009, is unchanged.
Exact locks and audit JSON are retained alongside both matrix summaries.

[nbreq 0.2.0](https://crates.io/crates/nbreq/0.2.0) and its
[API documentation](https://docs.rs/nbreq/0.2.0/nbreq/) are live. Ordinary consumers can use
`nbreq = "0.2"`; GDS can retire its local override when its own session chooses to rebuild.

## Publication observations and corrections

Dry runs used the normal Cargo publication path. The first controller looked for the Darwin
archive directly under `package/`; Cargo placed publication archives under `tmp-crate/` and
`tmp-registry/`. The successful dry run was reused and both staged archives were verified.
Root comparison separately identified and verified the harmless README line-ending difference;
it was not treated as a source change or silently omitted from the new package identity.

The first registry workflow (`34947171514`, source `6cfdf0a`) passed both Ubuntu jobs and all
eight package-verification stages, but its new extraction guard rejected six Windows/Mac temp
paths before consumer tests. It compared resolved destinations with an unresolved temp root
(Mac temp aliases and Windows short/long paths). Canonicalize the freshly created root too;
retain the containment check and both earlier path-component guards. This is a verification
runner correction, with no library/package change. The failed run and its artifacts are retained.

Pre-publication documentation check found the shared guide's relative examples link was emitted
unchanged into rustdoc, where `../examples/README.md` has no target. The guide now links to the
immutable examples index at `432509b` (the identical 0.2 examples). README links remain relative
for crates.io's normal rewrite. The root package/source identity was refreshed after this
one-line guide correction; the already-published helper packages remain unchanged. Rustdoc
was rebuilt from the exact final archive in an independent temporary workspace, and the
published page was checked afterward. The first attempt inside the main target directory hit
Cargo's parent-workspace check; its failed log and the corrected external run are retained.

The earlier registry-aware root candidate was 343,615 bytes, SHA256
`05d2504522350118ef702fb3aa01288bd5939da62dc5e1727807fd03813686ed`. Its differences from the
housekeeping archive were helper registry sources/checksums and README CRLF normalization only.
The final published archive differs from that candidate only in Git source metadata and the
one guide hyperlink. Dependency versions, runtime source, tests and README text did not change.
