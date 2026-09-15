# NBReq 0.2 clean candidate checkpoints

2026-09-10. Local preparation only: no push, tag or registry publication. The user-reported
Rust panic under Wine (W-01) is now reproduced and fixed in clean commit `149450d`; see
[the investigation](nbreq_wine_dns.md) for actual Wine/native proof and GDS handoff. The older
checkpoints below remain historical evidence. Final release acceptance is still open.

## Resume checkpoint

| Item | State |
| --- | --- |
| Darwin candidate | `codex/nbreq-darwin-0.1.0-release`, clean commit `4a6c807a49f72ad9ff6585b0f9b479b61fee6609`, parent `2c382b7`. |
| Root checkpoint | `codex/nbreq-0.2.0-release`, clean commit `7693d6cf6f702432235df4001b8ef1731d70a25e`, parent `4a6c807`. Review checkpoint; further changes may be needed after W-01 investigation. |
| Worktrees | `target/worktrees/nbreq-darwin-0.1.0-release` and `target/worktrees/nbreq-0.2.0-release`. Main remains at `2c382b7` with its existing dirty work. |
| Main/GDS boundary | Main's unrelated F5 edits and index were preserved. The narrow DNS fixture correction also exists in main. GDS source and its installed DLL were not changed. |
| Public docs | Root checkpoint removes temporary unreleased/publication-process wording from README, getting-started, migration and security docs. Main retains its unreleased wording pending integration. |
| Current gate | Exact root archive consumer passes on Windows stable and Rust 1.85: 64 test executions, eight feature probes and all seven examples. Fresh cached-index resolution with explicit Darwin override; not registry-only proof. All jobs finished. |
| Evidence | [Artifact manifest](evidence/nbreq_020_candidate_artifacts.json) and [91-file archive](evidence/nbreq-020-candidates-20260910.tar.gz), 480,289 bytes, SHA256 `62b8aac61a4bb1bcfb48410c9b6c82b5a7f4ca8c4335cca86375a67a4aff6986`. Every archived file rechecked against its hash. |
| W-01 successor | Clean `149450d6543376cdb8255b55d7eb286e98207f42`, branch `codex/wine-dns-adapters`, extends the root checkpoint. Scoped changes also exist in main. Clean root/winpoll packages and logs are in [W-01 artifacts](evidence/nbreq_wine_dns_artifacts.json). Winpoll 0.1.1 is a new publication prerequisite. |
| Next | As of 2026-09-14 the owner reports several successful live GDS Wine pilot days. Continue final soak, exact-candidate checks/hosted CI and registry-only proof; support/root publication remains separately authorized. Pilot provenance and explicit shutdown evidence are still unrecorded. |

## Frozen packages

| Package | SHA256 | Size |
| --- | --- | --- |
| `nbreq-darwin-0.1.0.crate` | `656cf22c96ceb3f0ab3b96b24a3465021e22483390a32cc34d201419674401c4` | 11,377 bytes; 8 files |
| `nbreq-0.2.0.crate` | `2e3374e0fed3fd6fb0bc23d2b5bead9a82e48900026bd2c51db2cd02b761e2a2` | 333,752 bytes; 82 files |

Both archives contain `.cargo_vcs_info.json` identifying the clean commits above, with no dirty
flag. Root archive contents exclude thoughts, evidence and private tools. Root Cargo.lock is
unchanged. Compared byte-for-byte with R1 root package B, only the four public documentation
files, `src/backend/native_dns/tests.rs` and VCS metadata differ. Runtime and dependency bytes
are unchanged from that earlier platform rehearsal.

The root package was built and verified with `--locked --offline` and an explicit command-line
Darwin override pointing to this candidate's `support/darwin`. It does **not** establish
registry-only resolution. An initial attempt to override with a different extracted helper
directory failed because Cargo would add an unused-patch lock entry; `--locked` prevented it.
Using the candidate's existing support path avoided changing the lock or source.

The external consumer instead uses both exact unpacked archives outside the source workspace,
with an explicit Darwin override. This distinction must remain visible until Darwin publication
allows a true registry-only graph.

Its offline resolution selects cached `bitflags 2.13.0` and `combine 4.6.7`, versus `2.13.2` and
`4.6.8` in the earlier online consumer rehearsal. Those are the only lockfile changes. This
checkpoint is therefore not a repeat of fresh online resolution. The cached consumer lock is
`57785d7670af10ab7ce0732f03a3b26d613d0bdcd5f3070e2de7e632d683ee8f` and has no reported
vulnerabilities or warnings against the previously frozen RustSec database; that follow-up
scan disabled the yanked-version check. Final registry/online acceptance remains open.

## Verification and the fixture finding

- Exact Darwin archive: Windows Cargo package verification passes. Both physical Macs verify
  its hash and clean commit, pass all five helper tests on stable and Rust 1.85, and pass stable
  Clippy with warnings denied. Commands are bounded and all remote jobs finished.
- Root Windows verifier: initial run failed at default tests when
  `route_generation_change_leaves_an_active_http_response_alive` joined a DNS fixture that had
  panicked on `recv_from` error 10054. The HTTP/route assertions themselves had completed.
- A bounded standalone Windows socket reproduction confirms that sending to a closed UDP port
  causes `ConnectionReset`/10054 on the sender's next receive; the same socket then receives a
  new client's packet successfully. The reproduction source and output are retained as evidence.
- The two long-lived native-DNS fixtures now continue on this error under `cfg(windows)`, matching
  the previously accepted public DNS fixture. Other receive errors still fail. No runtime DNS
  handling, timeout, assertion or success condition was relaxed.
- All six route-generation tests pass after the correction. The complete root Windows verifier
  then passes all 24 steps in 128.456 seconds, including the feature matrix, docs and pressure
  regressions. Original failure and successful follow-up logs are retained separately.
- The exact-archive Windows consumer passes default/native-only/minimal/test-support tests on
  stable and Rust 1.85, eight feature probes, and the three shared HTTP tests against registry
  0.1.1 on each toolchain. All seven packaged examples run, including live DNS and trusted HTTPS.
  The retained budget, streaming, TCP, cancellation and shutdown cases are included in those
  64 test executions; they do not replace the planned sustained mixed-workload soak.

Earlier R1/R2 platform, advisory and license evidence remains linked from the
[consumer-readiness report](nbreq_020_consumer_readiness.md). It must not be described as a new
full-matrix run of these exact commits. Nor does the fixture explanation dispose of the distinct
Wine panic reported by the user.

## W-01 and release boundaries

The subsequent [W-01 investigation](nbreq_wine_dns.md) reproduces the exact alignment panic
with unchanged ipconfig 0.3.4/widestring 1.2.1 on actual Wine 5 x86. Commit `149450d` replaces that
production dependency with bounded DNS discovery in winpoll 0.1.1. Native Windows and Wine
probes pass; GDS's deployed DLL/prefix and end-to-end acceptance remain open. Earlier package
identities in this document are preserved and must not be mistaken for the fixed binaries.

The Darwin support publication candidate is concrete, but publication remains a separately
authorized permanent action under the accepted [release checklist](nbreq_020_release_plan.md).
No approval has been requested or inferred from the instruction to continue preparation.
The root still has the final mixed HTTP/Resolver/TCP soak, final matrix/hosted-CI and x86 gates
as applicable, docs.rs/package-link checks, and registry-only package/consumer gates open.
General 128/256 MB device acceptance and GDS tuning remain outside this library checkpoint.

Do not reset or switch the dirty main checkout to integrate these branches. Reconcile its
overlapping accepted work and documentation deliberately when integration is scheduled.
