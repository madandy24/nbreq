# NBReq 0.2.1 final release

Opened 2026-09-27 after the owner accepted the documentation and authorized
housekeeping, main integration, final candidate checks, hosted CI and publication
after passing gates. Core only; helper versions remain unchanged and SMTP
publication/GDS integration stay separate.

## Resume checkpoint

| Item | State |
| --- | --- |
| Baseline | Reviewed release source `44bbd1b1badf33771efe264e433e2bdfdc6983d4`, pushed on main and tagged `v0.2.1` |
| Reviewed work | Documentation and bundled consumer tooling committed; owner approved consumer-lock compatibility policy below |
| Existing evidence | Documentation/defaults pass: full 30-step Windows verifier, stable/MSRV snippets, strict rustdoc and independent review; runtime/platform/Wine evidence remains in the linked plans |
| Current stage | Release complete; publication and consumer evidence independently accepted, with notes-only closeout on main |
| Publication | Core 0.2.1 published on 2026-09-27 at 07:35:03 UTC; API, sparse index and downloaded archive match the exact candidate |
| Local evidence | `target/release-021-20260927/` (commands, package hashes, consumer results and hosted receipts) |

## Gates

| Gate | Required result | State |
| --- | --- | --- |
| R1 Integration | Clean main containing the reviewed branch and accepted notes; no unrelated edits | Passed at `44bbd1b` |
| R2 Candidate | Exact clean package inventory, links, registry helper identities; packaged TLS regressions and bundle-on/off consumers | Passed; 114 members, 111 original files, 20 focused tests and both trust consumers; dry-run bytes identical |
| R3 Hosted acceptance | Eight platform/stable/MSRV verifier jobs plus advisory/license checks; candidate registry-helper consumer matrix | Passed; final CI 10/10 and candidate matrix 8/8; independent review accepted |
| R4 Publication | Final dated changelog, exact dry-run/archive identity, publish core 0.2.1, matching tag | Passed; core published and annotated tag pushed at the reviewed source |
| R5 Published acceptance | Registry API/index/download checksums, registry-only matrix, live README/docs.rs/versioned-link checks | Passed; exact archive, all eight published jobs and live documentation |
| R6 Closeout | Retained evidence, updated checkpoint and clean pushed main | Final evidence retained and member hashes verified; this closeout contains notes and evidence only |

Do not treat old package hashes as current-candidate evidence. Preserve failed
attempts and distinguish cached/offline, registry-helper candidate and fully
published-registry results. If the final date or another packaged file changes,
refresh the candidate identity and verify the exact delta before publication.

## Decisions and scope

Owner accepted on 2026-09-27: retain ordinary published dependency ranges and
Rust 1.85 support with an explicit application-lock workaround. Current stable
consumers use fresh unmodified resolution; Rust 1.85/1.86 consumers select
`yoke-derive 0.8.2` in their application lock. Rust 1.85 release jobs preserve
both initial and selected locks. Exact Rust 1.87 separately passed fresh consumer
checks before that upgrade route was documented. This deliberately
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

### Published release and final candidate (2026-09-27)

The released source is `44bbd1b1badf33771efe264e433e2bdfdc6983d4` with
annotated tag [v0.2.1](https://github.com/madandy24/nbreq/tree/v0.2.1).
[NBReq 0.2.1](https://crates.io/crates/nbreq/0.2.1) was published at
2026-09-27T07:35:03.525707Z. The exact archive contains 408,235 bytes and its
SHA256 is `fa12940c09ff6d3c61c32d4e4b885c8d229131cce9e9839f42c6b4d46ee46903`.
The registry API, sparse index and independently downloaded crate all match
that reviewed candidate. Helper versions are unchanged; SMTP remains unpublished.

Final [CI](https://github.com/madandy24/nbreq/actions/runs/36302391795)
passed all ten jobs. Independent review checked the raw logs for all 30 verifier
stages on Windows, Ubuntu, Intel macOS and Apple Silicon macOS, each with Rust
1.85.0 and hosted stable 1.98.1. Every verifier passed four configurations of the
one-byte TLS receive test. All nine CI artifact digests matched GitHub's records;
the advisory and frozen license checks passed under the existing documented
dev-only `RUSTSEC-2026-0009` exception. Local stable evidence used Rust 1.97.1.

The final [candidate matrix](https://github.com/madandy24/nbreq/actions/runs/36302422898)
passed eight jobs: 672 consumer tests, 32 expected negative feature probes and
144 examples across sixteen graphs. Eight graphs retained byte-identical fresh
locks; eight explicitly selected the old-compiler compatibility lock. Five
distinct selected/example locks passed advisory audits. Package, dry run and
both hosted gates received independent pre-publication acceptance with no
unresolved findings. Earlier failed attempts remain retained below.

The [published matrix](https://github.com/madandy24/nbreq/actions/runs/36303600781)
passed all eight jobs against the fully registry-sourced release. Raw receipts
confirm another 672 consumer tests, 32 expected negative probes and 144 example
runs. Every graph identifies the exact published NBReq checksum and registry
helper versions; the eight fresh graphs and eight compatibility-selected graphs
remain explicitly distinguished. All five distinct selected/example locks passed
advisory audits. Live checks verified the published README, Rust compatibility
guidance, six API pages and four versioned source guides, including the TLS guide
and example index. The locally recorded annotated tag object and its peeled
source commit match the remote tag exactly.

Independent post-publication review accepted the raw published-consumer logs,
all eight GitHub artifact digests, exact package and dependency identities,
advisory reports, tag and live documentation receipts with no unresolved findings.
The final [release evidence](evidence/nbreq-release-021-20260927.tar.gz) contains
320 regular members and 11,160,347 bytes; SHA256
`0942a4740776c9c5880a9ccea295b013711311cda6b82fd0e18a0885a69e03e9`.
The [artifact index](evidence/nbreq_release_021_artifacts.json) binds every member.
It retains final and superseded hosted results, package and publication logs,
registry identity checks, live-page receipts, dependency audits and both
independent review receipts. Every archive member was checked against its index.
The reviewed selection excludes build directories, the rejected broad diagnostic
upload and bridge files; the scoped credential/private-key scan found no matches.

The release tag remains at the published source. This final notes/evidence
closeout changes no packaged source files or dependency constraints. SMTP
publication, GDS deployment and the future timeout-default decision remain
separate work, rather than unfinished gates for core 0.2.1.

### Second hosted candidate (2026-09-27)

At `17a097dbb7096aeef42b6ab2acd96d63d1d4eb06`, the clean package and
publication dry run passed with identical archive SHA256
`f37e9ee96c3d296329364a1f4739812f5304a904ae38ce4375dea4066e7b9e32`.
Independent review verified all 114 archive members, VCS identity, 111 original
file hashes, registry helper requirements and raw packaged test/consumer results.
The [candidate matrix](https://github.com/madandy24/nbreq/actions/runs/36301613573)
passed eight jobs. [CI](https://github.com/madandy24/nbreq/actions/runs/36301585861)
passed nine jobs, including every Rust 1.85 job, advisory and license checks.

Intel stable failed the 15-second global deadline in the 65,537-byte TLS test
with a one-byte receive queue. Its log has no byte/progress count, so it cannot
distinguish stalled progress from a slow drain. The unchanged test passed in the
preceding Intel stable run and this run's Intel MSRV job. Do not claim a confirmed
host-performance cause.

Reviewed test-only repair: preserve exact bytes, one-byte queues, connection
timers, manual drive behavior, EOF and peer completion; use an eight-second
no-progress guard refreshed only after a validated byte, plus a 60-second total
watchdog. Report bytes, total elapsed and progress age on failure. Controlled
local diagnostic pacing fails the old timer at about 17 seconds and passes the
new guard with every byte/EOF/peer assertion. Withholding engine drives fails the
new guard after eight idle seconds at byte 8,192. Normal gates follow before
committing and refreshing the candidate. The old archive is superseded.

An optional dedicated-Mac source upload was rejected by automatic approval review
for broad internal source and insufficient destination-specific authorization.
No upload occurred. Local controlled diagnostics and hosted Intel verification
remain the validation route; omit the rejected broad archive and bridge files
from public evidence.

The normal stable verifier passed all 30 steps in 170.316 seconds; exact Rust
1.85 all-feature/all-target Clippy and 27 TLS integration/API tests passed too.
All 185 captured source/manifest/lock hashes remained unchanged. Retained narrow
[watchdog evidence](evidence/nbreq-tls-watchdog-021-20260927.tar.gz): 20 regular
members, 91,312 bytes, SHA256
`9572f8bd945c3e06baea815355a6fb6c80e5ea4121e8f5ebf9b4e171ae069d2e`.
The [artifact index](evidence/nbreq_tls_watchdog_021_artifacts.json) binds all
controlled probes and passing checks. Superseded package, dry-run and hosted
receipts are preserved under `target/release-021-20260927/attempt-2/`; its raw
candidate artifacts again verified 672 tests, 32 negative probes, 144 examples
and five advisory-clean selected/example locks.

### First hosted candidate (2026-09-27)

Pushed `465319bc44489045f353c52b6a019adb67e91ca4` and started
[CI](https://github.com/madandy24/nbreq/actions/runs/36300738053) and the
[candidate matrix](https://github.com/madandy24/nbreq/actions/runs/36300766614).
The clean local archive passed all packaged TLS checks and independent
bundle-off/on consumers. Its SHA256 is
`6f3adc15a21ab82f75cf48b19ebcdea29f7e02295e8428ae33d72c4d221c2034`;
it is superseded for publication by the following fixture correction.

Hosted Rust 1.85 Clippy rejects the test-fixture condition
`!encrypted && !(record[0] == 0x14 && length == 1)` as `nonminimal_bool`.
Current stable accepts it. Simplify the condition using De Morgan's law without
changing its evaluation or suppressing the lint. No production behavior changes.
Run the complete Rust 1.85 verifier and relevant current-stable checks, obtain
independent review, then build and gate a new clean candidate. Retain the original
hosted failures and package receipts as superseded evidence.

That candidate matrix completed all eight jobs: 672 consumer tests, 32 expected
negative feature probes and 144 examples. Raw receipts, sixteen graph identities
and five unique selected/example locks passed independent verification and
advisory audit. All four stable full-CI jobs, the advisory scan and license check
passed; the four MSRV jobs stopped at the fixture lint. Evidence is retained under
`target/release-021-20260927/attempt-1/`.

The local complete Rust 1.85 verifier passed the corrected fixture and all core
steps, then revealed `comparison_chain` in `smtp/src/data.rs`. Replace the
cursor/length if-chain with its equivalent `Ordering` match and verify the
existing encoding tests plus full verifier. Preserve that failed local run too.
SMTP remains unpublished; this change satisfies the existing workspace gate.

Both corrections passed code review. The second full Rust 1.85 verifier passed
all 30 steps in 108.437 seconds. Stable all-feature Clippy, TLS integration/API
tests, SMTP Clippy and all-target tests also passed with source hashes unchanged.
Retained [lint evidence](evidence/nbreq-msrv-lint-021-20260927.tar.gz): 32 regular
members, 141,406 bytes, SHA256
`78a832254bc862e79437c82fccb0c870d21f5cc565be565a28c997e889e76286`.
The [artifact index](evidence/nbreq_msrv_lint_021_artifacts.json) preserves the
original failure, exact changes, compiler identities and passing raw logs.

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
