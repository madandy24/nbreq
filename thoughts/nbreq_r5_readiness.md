# R5 release readiness

Opened 2026-09-15. The owner authorized starting R5, with a separate README/examples refresh
when work returns to main-tree integration. This report records the technical checks before
that presentation pass. Root/helper publication, registry-only acceptance and the final package
freeze remain separate gates in the [release plan](nbreq_020_release_plan.md).

## Resume checkpoint

| Field | State |
| --- | --- |
| Worktree | `target/worktrees/nbreq-r5`, branch `codex/r5-release`, based on pushed pre-R5 `620751b`. Preserve the main checkout's mixed work. |
| Runtime source | Unchanged from verified pre-R5 fix/package source `a44cac5`; no new production changes proposed. Root Cargo.lock remains the reviewed graph. |
| Hosted matrix | Preparing manual CI dispatch for stable/MSRV on Windows, Linux, Intel Mac and Apple Silicon Mac. Each job runs the complete verifier and two fresh consumer cases (ordinary and explicit newer Mio), retaining independent locks and output. Local helper overrides remain explicit. |
| x86 companion | Pending complete Windows i686 verifier against the same candidate source. |
| Advisories | Refresh root scan and fresh-consumer graphs; record audit version, advisory DB commit/date, lock hashes and applicable exceptions. Hosted audit now retains JSON and a separate DB identity. |
| Licenses | Regenerate the locked production license report and compare it with the committed artifact; evaluate any drift before acceptance. |
| README/examples | Owner's refresh is pending main-tree integration. Validate affected examples/doctests and freeze fresh final packages afterward. |
| Evidence | Dedicated local run directory `target/release-r5-20260915`; preserve failures and exact source identities. Do not repeat completed R4 four-hour soaks. |

The existing workflow only ran on main pushes, PRs or manual dispatch, so the pre-R5 branch push
did not supply hosted candidate proof. This pass adds a Windows MSRV job alongside the existing
seven verification jobs, fresh consumer resolution to all eight jobs, and evidence retention.
The official upload-artifact v4 action is pinned to verified commit
`ea165f8d65b6e75b540449e92b4886f43607fa02`. The private consumer runner gains toolchain selection
and source/platform metadata; its default two-toolchain behavior is unchanged.

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
