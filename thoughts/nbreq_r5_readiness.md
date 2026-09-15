# R5 release readiness

Opened 2026-09-15. The owner authorized starting R5, with a separate README/examples refresh
when work returns to main-tree integration. This report records the technical checks before
that presentation pass. Root/helper publication, registry-only acceptance and the final package
freeze remain separate gates in the [release plan](nbreq_020_release_plan.md).

## Resume checkpoint

| Field | State |
| --- | --- |
| Worktree | `target/worktrees/nbreq-r5`, branch `codex/r5-release`, based on pushed pre-R5 `620751b`. Preserve the main checkout's mixed work. |
| Runtime source | Rust implementation is unchanged from `a44cac5`; R5 now raises rustls to 0.23.45 after a newly published advisory. Current root/tool manifests and locks change accordingly. One existing test gains diagnostic context without weakening its assertion. |
| Hosted matrix | Initial commit `0f0d889` is running as GitHub Actions run `34930282926`. Its advisory job reproduces the root security finding; successful platform jobs remain evidence for that initial source, not the patched candidate. A separate patched-source run follows. Local helper overrides remain explicit. |
| x86 companion | Initial full verifier failed one all-feature HTTPS assertion (420/421 passed); the diagnostic all-feature rerun on the patched graph passed 421/421. Full patched verifier running. Preserve the original failure and investigate if it recurs. |
| Advisories | Root 0.23.42 scan fails on RUSTSEC-2026-0285. Patched root 0.23.45 scan passes against the same DB `e2e640471715167f73e22eaf761f2e547adafeec`, dated 2026-09-14T18:06:06+02:00, using cargo-audit 0.22.2. Fresh hosted consumer graph audits remain pending. |
| Licenses | Original graph regenerates identically. Patched graph regenerates successfully with the sole report change rustls 0.23.42 -> 0.23.45; apply that generated report and verify in hosted CI. |
| README/examples | Owner's refresh is pending main-tree integration. Validate affected examples/doctests and freeze fresh final packages afterward. |
| Evidence | Dedicated local run directory `target/release-r5-20260915`; preserve failures and exact source identities. Do not repeat completed R4 four-hour soaks. |

The existing workflow only ran on main pushes, PRs or manual dispatch, so the pre-R5 branch push
did not supply hosted candidate proof. This pass adds a Windows MSRV job alongside the existing
seven verification jobs, fresh consumer resolution to all eight jobs, and evidence retention.
The official upload-artifact v4 action is pinned to verified commit
`ea165f8d65b6e75b540449e92b4886f43607fa02`. The private consumer runner gains toolchain selection
and source/platform metadata; its default two-toolchain behavior is unchanged.

## S-01 — new rustls advisory

The September 15 root audit found [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html),
published September 14: affected rustls versions accept certain TLS 1.3 handshake messages at the
wrong encryption level. Upstream identifies 0.23.45 as patched. This is a real release blocker,
not an advisory exception. The runtime minimum changes to compatible `0.23.45`, the test fixture
pin changes to `=0.23.45`, and the current NBReq/tool locks move to that version. Earlier fresh
pre-R5 consumers already exercised 0.23.45, but final checks must use the updated candidate.

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
