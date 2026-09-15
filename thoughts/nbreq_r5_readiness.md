# R5 release readiness

Opened 2026-09-15. **Technical pass complete; README/examples refreshed; integration/final package gates remain.** The owner authorized starting R5, with a separate README/examples refresh
when work returns to main-tree integration. This report records the technical checks before
that presentation pass. Root/helper publication, registry-only acceptance and the final package
freeze remain separate gates in the [release plan](nbreq_020_release_plan.md).

## Resume checkpoint

| Field | State |
| --- | --- |
| Worktree | `target/worktrees/nbreq-r5`, branch `codex/r5-release`, based on pushed pre-R5 `620751b`. Preserve the main checkout's mixed work. |
| Runtime source | Rust implementation is unchanged from `a44cac5`; R5 now raises rustls to 0.23.45 after a newly published advisory. Current root/tool manifests and locks change accordingly. One existing test gains diagnostic context without weakening its assertion. |
| Hosted matrix | Patched source **`bdf5db590e4da6ee5a1534f408a370787453cff5`** passes all 10 jobs in run `34930899817`: eight complete platform/stable/MSRV verifiers plus consumer graphs, advisory and license jobs. Initial run `34930282926` passed its eight verification jobs and license job; only the newly discovered advisory failed. Both runs are retained. Local helper overrides remain explicit. |
| x86 companion | Patched source passes the complete Windows i686 verifier **24/24 in 107.003 s** on local Rust 1.97.1. Initial full verifier failed one all-feature HTTPS assertion (420/421 passed); diagnostic rerun passed 421/421. Preserve that unconfirmed initial failure and inspect the richer diagnostic if it recurs. |
| Advisories | Root 0.23.42 scan fails on RUSTSEC-2026-0285. Patched root and both distinct final consumer locks pass with no reported vulnerabilities/warnings against DB `e2e640471715167f73e22eaf761f2e547adafeec`, dated 2026-09-14T18:06:06+02:00, cargo-audit 0.22.2. Existing test-only time exception remains documented; no new exception was added. |
| Licenses | Original graph regenerates identically. Patched generated report changes only rustls 0.23.42 -> 0.23.45 and passes the hosted byte-for-byte regeneration check. |
| README/examples | Owner-approved 17-program A/B/C refresh and consumer-focused README are implemented and mirrored to main. [Examples](nbreq_examples_refresh.md) retain Windows stable/MSRV, local/live and normalized-package checks; the [README](nbreq_readme_refresh.md) has four compiled snippets and verified package links/anchors. Main integration, updated hosted CI and final clean package freeze remain. |
| Evidence | [Manifest](evidence/nbreq_r5_artifacts.json) and [299-file sealed archive](evidence/nbreq-r5-technical-20260915.tar.gz), 4,629,805 bytes, SHA256 `f6f02579763add7843756e8d489a2d44c3f531e04acf13751558c392cacc574f`. Raw working directory `target/release-r5-20260915`. Completed R4 four-hour soaks were not repeated. |
| Remote checkpoint | Code/workflow commit `bdf5db5` is already pushed and hosted proof is public. Automatic approval review rejected pushing the new 4.63 MB evidence archive, stating that the earlier owner approval covered a different payload. Final report/archive commit remains local pending explicit approval for this archive and the same public `madandy24/nbreq` destination. This does not block the README/examples discussion. |

The existing workflow only ran on main pushes, PRs or manual dispatch, so the pre-R5 branch push
did not supply hosted candidate proof. This pass adds a Windows MSRV job alongside the existing
seven verification jobs, fresh consumer resolution to all eight jobs, and evidence retention.
The official upload-artifact v4 action is pinned to verified commit
`ea165f8d65b6e75b540449e92b4886f43607fa02`. The private consumer runner gains toolchain selection
and source/platform metadata; its default two-toolchain behavior is unchanged.

## Final hosted and consumer evidence

[Passing hosted run](https://github.com/madandy24/nbreq/actions/runs/34930899817) checks exact source
`bdf5db5`. Hosted stable Rust is **1.98.1** and the minimum version is **1.85.0**, on Windows x64,
Linux x64, Intel Mac x64 and Apple Silicon arm64. All eight logs explicitly report the complete
24-step verifier passing. The local Windows i686 companion used Rust **1.97.1**, with its explicit
target recorded separately.

Every hosted verification job also passed two independent fresh consumer cases, ordinary and
Mio-1.2.3 coexistence. Total: **136 consumer stages**, **16 cases**, **464 test executions**,
**32 expected negative API-visibility probes**. Each case exercised default, native-only,
minimal and test-support modes. All selected rustls **0.23.45**. The 16 retained locks reduce to
two distinct byte-identical groups; both were separately audited successfully against the frozen
DB. Source hashes in every artifact match the dispatched Git inputs, accounting for checkout
line endings. Host compiler/architecture records, logs, locks and both hosted runs are sealed.

Seven current root/tool graphs also resolve with `--locked --offline` and rustls 0.23.45 without
lock changes. The Wine probe deliberately keeps a direct old ipconfig dependency to reproduce
the historical fault; NBReq's own dependency graph does not include it. One preliminary check
needed missing crate downloads; another overbroad probe assertion was corrected. Those were
verification-tool issues, not changes to the candidate's dependencies or runtime acceptance.

The scoped changes were mirrored into main after content checks, preserving its unreleased
notices and otherwise different F5 tool dependency versions. That tool's lock was updated in
place only for rustls and the already-accepted Wine graph. Main remains unmerged and uncommitted.
Consumers adopting the new minimum may need to update their own Cargo.lock; GDS rebuild and
application acceptance remain owned by the GDS work.

## S-01 — new rustls advisory

The September 15 root audit found [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html),
published September 14: affected rustls versions accept certain TLS 1.3 handshake messages at the
wrong encryption level. Upstream identifies 0.23.45 as patched. This is a real release blocker,
not an advisory exception. The runtime minimum changes to compatible `0.23.45`, the test fixture
pin changes to `=0.23.45`, and the current NBReq/tool locks move to that version. Earlier fresh
pre-R5 consumers already exercised 0.23.45, but final checks must use the updated candidate.
The final matrix above now supplies that proof. A Rust-1.85 resolution-only probe additionally
confirms that a consumer requiring affected rustls 0.23.44 is refused by the new minimum; no
affected code was built for that probe.

Root Cargo.lock changes only rustls's version and checksum. Updating current fuzz, memory and
physical-Mac tool locks also reconciles their stale pre-Wine graph (removing ipconfig, widestring
and socket2, and selecting winpoll 0.1.1). No unrelated dependency upgrade was requested. The
registry-0.1.1 historical comparison lock and retired curl experiment remain outside this release
graph; previous sealed evidence is unchanged. Pre-R5 root packages must not be published because
their manifests still permit affected rustls versions; freeze new root packages after the owner's
README/examples pass.

## X-01 — initial Windows x86 HTTPS assertion

The first complete verifier, on the original graph, passed default/native checks and failed
`resolved_https_verifies_hostname_and_preserves_explicit_bypass` during all-feature tests. The
wrong-host request returned an error without a TLS stage; the old assertion did not print the
actual error, so its cause is unconfirmed. This test has two-second operation deadlines, but
the original output does not prove a timeout. The existing TLS-stage requirement is retained
and now reports the full structured error on failure. The patched all-feature diagnostic run
passed all 421 tests; that does not establish that the dependency update fixed this failure.
Do not silently relabel the initial failure as a library regression or dismiss it as host load.
The complete patched x86 verifier and all eight patched hosted verifiers subsequently passed.
This isolated initial failure remains an explicit limitation in the release evidence; no timing
limit or TLS assertion was relaxed to obtain a pass.

## Order before the owner's presentation pass

1. Obtain hosted platform/stable/MSRV evidence and the Windows x86 companion; investigate any
   failures before treating the candidate as accepted.
2. Refresh advisory and license evidence, including the newer graphs selected by consumers.
3. Reconcile any resulting code/manifest fixes and the isolated candidate with main's owned work.
4. Carry out the owner's README/examples refresh, verify the affected artifacts, and build final
   clean packages from that integrated source.

Both helper crates still require separate publication authorization. Their actual registry
availability must be proved before the root registry-only consumer gate. Final published README
and docs.rs checks remain in R6. Neither helper publication nor the README refresh is required
to start the technical checks above.
