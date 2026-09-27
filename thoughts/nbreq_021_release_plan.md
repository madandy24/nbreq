# NBReq 0.2.1 final release

Opened 2026-09-27 after the owner accepted the documentation and authorized
housekeeping, main integration, final candidate checks, hosted CI and publication
after passing gates. Core only; helper versions remain unchanged and SMTP
publication/GDS integration stay separate.

## Resume checkpoint

| Item | State |
| --- | --- |
| Baseline | Remote main remains published 0.2.0 completion commit `6ada116`; local main integrated through `55cb6d9` |
| Reviewed work | Documentation and bundled consumer tooling committed; owner approved consumer-lock compatibility policy below |
| Existing evidence | Documentation/defaults pass: full 30-step Windows verifier, stable/MSRV snippets, strict rustdoc and independent review; runtime/platform/Wine evidence remains in the linked plans |
| Current stage | Consumer-lock policy implemented and reviewed; local gates passed, preparing clean candidate and hosted checks |
| Publication | Not started; v0.2.1 tag absent at initial remote check |
| Local evidence | `target/release-021-20260927/` (commands, package hashes, consumer results and hosted receipts) |

## Gates

| Gate | Required result | State |
| --- | --- | --- |
| R1 Integration | Clean main containing the reviewed branch and accepted notes; no unrelated edits | Initial integration complete at `b2427a0`; final tooling/date commit follows |
| R2 Candidate | Exact clean package inventory, links, registry helper identities; packaged TLS regressions and bundle-on/off consumers | Local policy and full verifier passed; exact clean package follows final commit |
| R3 Hosted acceptance | Eight platform/stable/MSRV verifier jobs plus advisory/license checks; candidate registry-helper consumer matrix | Pending |
| R4 Publication | Final dated changelog, exact dry-run/archive identity, publish core 0.2.1, matching tag | Pending |
| R5 Published acceptance | Registry API/index/download checksums, registry-only matrix, live README/docs.rs/versioned-link checks | Pending |
| R6 Closeout | Retained evidence, updated checkpoint and clean pushed main | Pending |

Do not treat old package hashes as current-candidate evidence. Preserve failed
attempts and distinguish cached/offline, registry-helper candidate and fully
published-registry results. If the final date or another packaged file changes,
refresh the candidate identity and verify the exact delta before publication.

## Decisions and scope

Owner accepted on 2026-09-27: retain ordinary published dependency ranges and
Rust 1.85 support with an explicit application-lock workaround. Current stable
consumers use fresh unmodified resolution; Rust 1.85 consumers and release jobs
select `yoke-derive 0.8.2`, preserving both initial and selected locks. Test Rust
1.87 separately before recommending that exact upgrade route. This deliberately
replaces the earlier requirement for unaided fresh resolution on the minimum
compiler; it does not relabel the original failed graph as a pass. Do not add a
library-wide pin or change core dependencies. Retire the workaround after an
upstream-compatible correction and fresh validation.

- Existing timeout defaults remain unchanged; the
  [future-defaults item](nbreq_021_documentation_plan.md#future-revision-sensible-timeout-defaults-throughout)
  requires a deliberate compatibility decision.
- Platform verification remains default. Portable WebPKI is explicit; Wine 5
  platform trust remains unsupported by the recorded evidence. The existing
  Win32/Wine proof depends on its documented app-local shim and environment.
- ARM live SMTP timeout remains a qualified live observation. Local TLS protocol
  tests and other verified hosts are separate evidence; do not invent a cause or
  erase the observation.
- No SMTP publication, GDS deployment, system trust-store change or new feature
  is included in this release operation.

## Prior evidence

### Fresh dependency blocker (2026-09-27)

The expanded consumer runner passes fresh stable graphs both alone and with Mio
coexistence. Its first fresh Rust 1.85 case fails before the bundled mode:
`yoke-derive 0.8.3` calls primitive `str::from_utf8` at `src/lib.rs:202`, which is
unavailable on Rust 1.85. This is upstream source compilation, not an NBReq or
TLS runtime failure. The checked-in working graph selects `yoke-derive 0.8.2`.
Public registry metadata currently lists 0.8.3 as latest, non-yanked, with no
`rust_version`, so compiler-aware fallback cannot reject it on that metadata.

The original fresh lock and failing log are preserved at
`target/release-bundle-consumer-20260927/pre-r5/1.85.0-fresh/` and
`target/release-bundle-consumer-20260927/pre-r5/1.85.0-fresh-default.log`.
Diagnostics using an older lock or a precise downgrade must remain explicitly
qualified and cannot be presented as fresh-consumer acceptance.

Owner input requested: use a documented temporary native-enabled constraint to
the already-transitive compile-time `yoke-derive 0.8.2`, or hold publication for
an upstream correction. Preserve Rust 1.85 and ordinary runtime ranges in either
case. No registry-cache patch, hidden consumer pin, MSRV increase or publication
is authorized by this diagnostic. Changelog date remains pending until the
release path is clear. No new version or tag has been published.

At that checkpoint the owner requested more explanation, rather than selecting the guard. Explained
the URL/IDNA/ICU dependency chain, build-time-only role, missing MSRV metadata and
the possibility of version conflicts from a temporary guard. Keep that decision
pending at that point; the later accepted consumer-lock policy is recorded above.
No core manifest/lock change has been made.

Tooling checks: fresh stable + Mio graphs pass 84 tests; the reviewed lock passes
40 default/bundle tests across stable and Rust 1.85; unchanged legacy 0.1.1 tests
pass six cases with their historical lock. A separate copy of the failing fresh
graph, with only a precise `yoke-derive 0.8.2` downgrade and its resolver-required
`synstructure 0.13.2` addition, passes all five modes / 42 tests on Rust 1.85.
The original failing lock remains byte-identical. This proves the isolated
workaround, not a published-manifest guard or unrestricted fresh MSRV acceptance.

Retained [consumer evidence](evidence/nbreq-release-consumer-021-20260927.tar.gz):
173 regular members, 407,458 bytes, SHA256
`d68e8b98ceed9bc6f1c2203f4948f51c31cf84ce8018a989d4f86be9da95fd71`.
The [artifact index](evidence/nbreq_release_consumer_021_artifacts.json) includes
raw failures, successful diagnostics, exact source/lock hashes, registry metadata
and the passing workspace audit. No build binaries are included.

### Review and supporting checks

The accepted two-route policy is now implemented in the consumer runners and
workflow artifact capture. Exact Rust 1.87 was installed alongside the existing
toolchains without changing the default. Local `pre_r5.py` completes 65 steps /
six graphs on Rust 1.85, stable and Rust 1.87, each alone and with Mio coexistence.
Old-compiler records show explicit compatibility selection; both newer routes
retain byte-identical fresh initial/selected locks. No library dependency pin
was added. These six graphs pass 252 tests. The archive runner passes 90 current
and legacy consumer tests plus 18 examples against the historical `9e81021`
rehearsal packages; this validates tooling, not the final release candidate.
Independent review accepted the source, raw lock receipts and test counts.

The initial full main verifier failed seven SMTP fixture tests while other
agent-managed Rust builds were active. Three report connect timeouts; four report
completion before their intended protocol gate. The same unchanged SMTP tests
then passed twice with ordinary parallel execution after those builds completed.
One complete quiet verifier subsequently passed all 30 steps in 154.765 seconds,
with source/documentation hashes unchanged. Contention remains a hypothesis,
not a confirmed cause. Retain the initial failure; investigate any recurrence in
hosted acceptance before release. No fixture or runtime behavior was changed.

Retained [policy and verifier evidence](evidence/nbreq-consumer-policy-021-20260927.tar.gz):
268 regular members, 641,213 bytes, SHA256
`4259e346b372dc32597dfc885e1f50845ff2bd78c0db174e74ab79506ea9e188`.
The [artifact index](evidence/nbreq_consumer_policy_021_artifacts.json) binds the
original/selected locks, compiler identities, source hashes and all three test
observations. Raw policy evidence remains in `target/consumer-policy-20260927/`.

Release review found that the fresh independent-consumer modes did not select
`bundled-roots`. Add that feature/mode and metadata proof across local and registry
runners before freezing the candidate, with stable/MSRV coverage and independent
review. Existing locked all-feature tests and the packaged supplied-root TLS
examples already passed; those do not substitute for the fresh bundled graph.
Set the intended changelog date before packaging and confirm it again at publication.
Audit unique fresh consumer locks as well as the workspace lock.

- [Documentation and examples](nbreq_021_documentation_plan.md)
- [API/version decision](nbreq_release_api_review.md)
- [Portable TLS and platform proof](nbreq_portable_tls_plan.md)
- [HTTP timing-test repair](nbreq_http_timeout_test_plan.md)
- [Previous publication procedure](nbreq_020_publication.md)
