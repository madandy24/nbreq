# NBReq 0.3.0 pre-release checks

Opened 2026-09-29 after the owner accepted the README/example simplification and
requested standard tests, CI and processes prior to release. Commit/push and
candidate CI are authorised. Publication and release-tag creation are not part
of this request; return the concrete candidate and any remaining decision first.

## Resume checkpoint

| Item | State |
| --- | --- |
| Baseline | Initial main ca08014; pre-release proof a64bf92; final dated release source ae7bfeed7cc0bffa0e5e04cf16900496a1b66f08 |
| Scope | 0.3.0 timeout defaults plus accepted documentation/examples and any demonstrated test correction; no new runtime work |
| Current stage | Ready to publish: dated source, final checks, independent review and retained evidence complete; awaiting owner instruction to publish/tag |
| Source | Approved example edits plus neutral version prose, versioned guide links and package checks |
| Local lab | Final: target/release-030-final-20260930; earlier evidence preserved under target/release-030-20260929 |
| Canonical package | 414,961 bytes; SHA256 193601f9a1616659afcd1802d3f904dded739a73ffc4786fffb828feb102f242 |
| Publication checkout | C:/Users/andre/.codex/worktrees/nbreq-release-030/nbreq; clean at exact dated source; reproducible canonical bytes |
| Publication | Held; no publish or tag action authorised by this pass |

## Gates

| Gate | Required result | State |
| --- | --- | --- |
| P1 Local/integration | Full 30-stage verifier, clean reviewed commit, pushed main | Passed 30/30 in 149.277 seconds; final source ae7bfeed pushed |
| P2 Candidate | Exact package inventory/links, registry helpers, unpacked timeout/TLS regressions, independent stable/MSRV consumers, dry run | Passed; 116 package members, 89 unpacked tests/doctests per compiler, 46 consumer steps; dry-run and clean publication-checkout bytes match canonical |
| P3 Hosted CI | Eight platform/compiler verifier jobs plus advisory and license jobs | Passed 10/10, run 36560189498 on ae7bfeed |
| P4 Hosted candidate | Eight platform/compiler candidate jobs using published helpers; retained logs, locks and artifact digests | Prior 8/8 run 36555080515 on a64bf92 carried forward by exact package-member comparison: only two documentation files and VCS metadata differ |
| P5 Review/closeout | Independent acceptance, retained evidence, ready-to-publish checkpoint and remaining decisions | Passed; final readiness and exact retention inventory independently accepted; ten distinct dependency locks freshly audited; final archive below |

Use a single local Cargo lane. Avoid reusing an xtask build directory across
different source roots: its compile-time workspace path caused a rejected stale
run in the defaults pass. Verify the actual logged root and source hashes. Remote
host results from ca08014 remain prior implementation evidence, not proof of the
later documentation/example candidate. This pass uses hosted CI for that final
platform matrix; use private hosts only if a concrete failure needs investigation.

## Release considerations

- 0.3 is the deliberately accepted behavioural boundary. Existing `"0.2"`
  applications remain there until explicitly migrated. `None` opts out per timer;
  quiet established TCP/TLS stays open, and fresh HTTP streams have no total cap.
- Rust 1.85/1.86 retains the documented consumer-lock selection. Stable graphs
  resolve freshly; preserve initial and compatibility-selected MSRV locks rather
  than calling the original graph a pass. No library-wide dependency pin is added.
- Published helpers remain nbreq-winpoll 0.1.1 and nbreq-darwin 0.1.0. Verify their
  registry identity; local workspace overrides are not sufficient release proof.
- Neutral 0.3 documentation prose prepares the candidate without announcing
  publication. Current guide links target the eventual v0.3.0 tag; validate their
  paths/anchors locally now and their live URLs after an authorised tag exists.
  Historical 0.2 migration links stay pinned to v0.2.1.
- CHANGELOG is dated 2026-09-30 in the frozen release commit. Its documentation
  delta and new archive identity are verified below. Any further packaged edit
  requires a refreshed candidate; publish from the recorded frozen source.
- No GDS migration/deployment or SMTP publication is included. Existing Wine and
  live SMTP qualifications remain; this pass does not widen those claims.
- Recheck advisories and licenses on the locked graph and every distinct selected
  consumer/example graph. Preserve the narrowly documented dev-only time exception;
  do not add exceptions to make a gate pass.

## Preparation findings

Independent review requested neutral version text, a version-neutral rustls floor
in SECURITY, versioned guide links, packaged Markdown checks covering the new
migration/TLS guides, and actual execution of unpacked `timeout_defaults` tests.
These refinements precede source freeze. The current v030 external API test adds
five passes per graph: derive actual counts from logs; the eight-job candidate
matrix is expected to contain 752 test passes, 32 expected negative probes and
144 local example executions, not the old 672-test total.

## Execution notes

- Fresh workspace RustSec audit passed against database commit
  `f23b768236fe2880e4cfa167da662cad8ca79240` (2026-09-29). The existing dev-only
  exception is unchanged; the final ten-lock scan is recorded below.
- The first required verifier stopped at native-only tests: the TLS timeout /
  cancellation test received a disconnected fixture event channel. Preserve
  `target/release-030-20260929/verify/`, including the unchanged tracked-source
  hash receipts. The only new untracked file was this excluded planning document.
- Review found the timeout case incorrectly demands server acceptance despite a
  150 ms admission-based direct-TLS deadline that also bounds TCP establishment.
  The original log does not locate which of the two accept-event calls failed;
  do not attribute the observed failure to host load as an established fact.
  Isolate the TLS timer, retain an accepted-socket upgrade timeout case and the
  accepted cancellation case, then independently review and rerun the full gate.
- The reviewed test correction disables the direct case's other operation timers,
  checks `TimeoutKind::Connect`, adds an already accepted plain-TCP-to-TLS timeout
  case, and retains accepted cancellation and zero-resource assertions. No runtime
  code changed. The focused test passed, followed by all 30 verifier stages in
  231.843 seconds with unchanged tracked-source hashes and backtraces enabled.
  The retry reuses only the compiled cache for the same source root; its receipts
  are separate in `verify-retry/`. No tests were skipped or serialised for the retry.
- The canonical package will be generated without local helper overrides. The
  helper-patched rehearsal is separately labelled; only the canonical archive is
  used for unpacked tests and independent registry-helper consumers. The publish
  dry run must independently produce identical size and SHA256 with clean VCS
  provenance. Future v0.3.0 documentation paths and anchors are checked locally.
- First integrated candidate: `ae114cf7d97c0764a9465d7399a2014191bedf67`, pushed to main.
  [CI](https://github.com/madandy24/nbreq/actions/runs/36552426679) and
  [candidate consumers](https://github.com/madandy24/nbreq/actions/runs/36552453885)
  were started on that exact commit. The canonical archive is 413,680 bytes,
  SHA256 `eb17b19648af63a1caa63e5a299aaa656985396206aa639e7e70e628c3e7bf9c`.
  Independent review verified all 116 members and committed source provenance.
- The initial unpacked-test launch was incorrectly nested below the workspace;
  Cargo rejected workspace membership before compilation. Preserve that harness
  failure in `packaged-tests/`. The corrected `packaged-tests-retry/` extracts the
  unchanged archive outside the checkout and records its path and hashes. Stable
  and Rust 1.85 passed timeout/defaults, TCP/TLS, portable trust and TLS shutdown
  checks. No manifest patch was used to bypass the workspace issue.
- Hosted Intel Mac/Rust 1.85 failed the protocol-composition HTTP test with an
  explicit two-second total timeout; 389 other native-only unit tests passed.
  The failure establishes deadline expiry, not whether progress was slow or
  stalled. Review requested bounded fixture I/O/progress diagnostics and a more
  generous finite total watchdog with a shorter inactivity limit, preserving
  bytewise response writes and every protocol assertion. Registry/dry-run work
  on the superseded candidate is held while that test correction is prepared.
- Hosted candidate jobs build independently from the same clean commit. Record
  their per-job package hash receipts; archive bytes may differ by platform or
  compiler. The workflow retains those receipts, not the remote archive bytes.
  Local canonical and dry-run bytes are available for independent hashing; do
  not claim cross-platform archive byte identity from hosted receipts alone.
- The first hosted candidate matrix passed all eight jobs. Independent artifact
  review confirmed 752 consumer tests, 32 expected feature-absence failures,
  80 executions of the new optional-timeout API case, and 144 local examples.
  The first standard CI run completed 9/10; only the recorded Intel Mac MSRV HTTP
  test failed. All raw logs/artifacts are retained under the parent lab.
- The HTTP test correction preserves all serialization, interim-response, chunked
  body, status and header assertions. Its fixture has a 20-second absolute bound,
  the request a 15-second total and 5-second inactivity watchdog, and client errors
  report acceptance, byte progress and elapsed time. These are test watchdogs,
  not public API defaults. Runtime code is unchanged.
- A temporary controlled variant proves a paced response can complete after
  3.461 seconds (137 bytes) while a silent response fails with Inactivity after
  5.008 seconds (zero response bytes). The first paced diagnostic accidentally
  sent `paced` to a branch expecting `pace`; its 0.01-second normal-path pass is
  explicitly rejected in `attempt-2/rejected-paced-control.json`. Corrected modes
  fail closed and print their measured outcome. Preserve both original and
  corrected evidence, including source snapshots and diffs.
- An intermediate full verifier was interrupted while that diagnostic mismatch
  was investigated; it is not a completed gate. The accepted test source was
  restored byte-for-byte (`cee6daf04f48f2061042e0c0e154c4985fab02a59a1b36d6e230d02464e475ec`).
  The fresh `attempt-2/verify-retry/` then passed all 30 stages in 273.925 seconds
  with tracked-source hashes unchanged. Only that completed run counts for the
  final HTTP test correction. Independent review accepted the corrected controls.

## Accepted pre-release checkpoint: 2026-09-29

The reviewed source is `a64bf92bd74c871cb77e8eac5c4e9e0cde91a0b1`.
The completed precommit verifier's 473 tracked-file hashes match that commit,
allowing only the excluded planning-note update and normal checkout line endings.
Subsequent notes/evidence-only commits do not change the tested source. They do
change the Git identity, so any package prepared from a later commit needs its
own provenance and archive hash.

- [Final CI](https://github.com/madandy24/nbreq/actions/runs/36555053067): 10/10
  jobs passed. Windows, Ubuntu, Intel macOS and Apple Silicon macOS each passed
  the full 30-stage verifier on stable Rust 1.98.1 and Rust 1.85.0. Advisory and
  generated-license checks passed. The local verifier used Rust 1.97.1.
- [Final candidate consumers](https://github.com/madandy24/nbreq/actions/runs/36555080515):
  8/8 jobs passed on those platform/compiler combinations. Each hosted matrix
  independently records 16 consumer graphs, 752 test passes, 32 expected
  feature-absence failures, 80 optional-timeout API passes and 144 local example
  executions. All 17 downloaded artifact digests matched GitHub's receipts.
- The canonical package has 116 safe regular members; all 113 nongenerated files
  match committed source. Its SHA256 is
  `b84102cabb72214305bddb4f365d093b3fdb17cee7cf3fa2e1620d28f8034b1a` and size
  is 414,339 bytes. Future-tag documentation paths/anchors and relative links
  passed. Unpacked stable/MSRV runs passed 51 focused tests per compiler;
  post-run extraction hashes still match all 116 archive members.
- Independent local consumers passed 46 steps across four graphs: 188 test
  passes, eight expected negative probes, 20 optional-timeout API passes and
  18 local examples. Published helper dependencies are verified; no root
  manifest patch or library-wide dependency pin was introduced.
- `cargo publish --dry-run --locked -p nbreq` succeeded without uploading.
  Its first identity wrapper looked in the wrong output directory; preserve
  that wrapper failure. The final receipt binds the successful command and
  unchanged source snapshots to both actual generated archives, each exactly
  identical to the canonical package.
- All ten distinct workspace/consumer/example locks passed cargo-audit 0.22.2
  against database `f23b768236fe2880e4cfa167da662cad8ca79240`: zero unignored
  vulnerabilities and zero warnings, with only the existing dev-only exception.
  The initial audit wrapper incorrectly required the DB commit in JSON where
  explicit local `--no-fetch` scans return null. Preserve that failure; the
  accepted scan binds the clean database Git identity before and after instead.
- Independent source and raw-evidence review accepted these results with no
  remaining code blocker. Both test repairs leave production code unchanged.
  Examples use local fixtures; these runs add no live DNS, Wine or deployment
  claims. Hosted archive-byte limitations remain as recorded above.

Retained [release evidence](evidence/nbreq-release-030-20260929.tar.gz):
464 regular members, 9,330,295 bytes, SHA256
`4cd4e8296f45ac7f39dbbf124a922c7230753f9ebf1f97e055d6511197bf9810`.
The [artifact index](evidence/nbreq_release_030_artifacts.json) binds the source,
canonical package and independently reviewed inventory. It retains accepted
results, failed/superseded attempts, corrected and rejected diagnostic controls,
raw hosted artifacts, selected locks, audit reports and the final review receipt.
The archive references the earlier four-host defaults evidence by hash; no build
trees, private bridge configuration or credentials are included.

## Publication handoff

### Final documentation pass opened 2026-09-30

The owner accepted the external review and requested the small documentation
changes followed by final checks, returning when ready to publish. Date the
changelog 2026-09-30, promote the streaming/inactivity/conversion and direct-TLS
deadline gotchas, and clarify zero-duration processing in the migration guide.
No runtime, dependency, SMTP or GDS changes are intended. Use a fresh lab at
`target/release-030-final-20260930`; preserve the 2026-09-29 evidence unchanged.
Review the documentation delta, run the required precommit verifier, freeze the
dated commit, then verify the exact package and dry-run bytes, selected dependency
graphs and fresh advisories. Publication and release tags remain held.

### Final dated release checks: 2026-09-30

The release source is frozen at `ae7bfeed7cc0bffa0e5e04cf16900496a1b66f08`.
Its changelog is dated 2026-09-30 and calls out stream inactivity, conversion and
complete-options defaults, combined direct-TLS deadlines, and explicit `None`.
The migration guide clarifies zero-duration timers, connected TCP phases and
backpressure. No runtime or dependency change was made in this final pass.

- The required local verifier passed all 30 stages in 149.277 seconds. All 475
  tracked hashes were unchanged and independently bound to the final commit.
- [Final dated-source CI](https://github.com/madandy24/nbreq/actions/runs/36560189498)
  passed 10/10. All four platforms passed the full verifier on stable Rust 1.98.1
  and Rust 1.85.0; license and advisory jobs passed. Its 16 consumer graphs
  recorded 752 positive tests, 32 expected feature-absence failures, 80 optional
  timeout API passes and 144 local example executions. All nine artifact digests
  and raw test counts were independently verified.
- The canonical crate is 414,961 bytes, SHA256
  `193601f9a1616659afcd1802d3f904dded739a73ffc4786fffb828feb102f242`, with 116
  regular members. Comparison with the previous candidate changes exactly
  `CHANGELOG.md`, `docs/migrating-to-0.3.md` and `.cargo_vcs_info.json`. Runtime,
  manifests, dependency lock, tests and examples are byte-identical. This supports
  carrying forward the prior eight-host candidate matrix; that matrix did not
  run against these new archive bytes.
- Unpacked stable Rust 1.97.1 and exact Rust 1.85 each passed 89 tests/doctests.
  All 116 extracted source files still match the archive after execution.
  The final local registry-helper matrix passed 46 steps: 188 positive tests,
  eight expected negative probes, 20 optional-timeout API passes and 18 examples.
  Both actual publication dry-run archives are byte-identical to the canonical
  package; the command exited successfully without uploading.
- A fresh official RustSec database fetch still resolved to clean commit
  `f23b768236fe2880e4cfa167da662cad8ca79240`. The union of previous and final
  selected locks remains ten distinct graphs; all ten passed cargo-audit 0.22.2
  with zero unignored vulnerabilities or warnings. The existing dev-only exception
  is unchanged. Public index checks found 0.3.0 absent and both support-crate
  versions non-yanked with expected checksums. An initial expected-checksum
  transcription error in that preflight is preserved with its corrected receipt.
- The attached clean publication checkout is
  `C:/Users/andre/.codex/worktrees/nbreq-release-030/nbreq`. Its packaging-only
  replay produces the exact canonical bytes. Initial Windows checkout conversion
  changed line endings in two licenses and 17 text fuzz seeds. Restoring only
  those files to verified Git blob bytes and refreshing their index bookkeeping
  preserved every index object/mode and all other file contents; ordinary Git
  status is clean. Failed comparison and rolled-back first normalization attempts
  are retained. No Git configuration, source commit or dirty-check bypass changed.

Final readiness was independently accepted, including the raw CI/audit evidence,
exact package delta, dry-run archives and clean publication checkout. Retained
[final-release evidence](evidence/nbreq-release-030-final-20260930.tar.gz):
248 regular members, 3,959,619 bytes, SHA256
`fbceb2cfdf7c4132f6c22bf5431af4c9171adf22ce0a0a5b2c36b47b17438412`.
The [final artifact index](evidence/nbreq_release_030_final_artifacts.json) binds
the frozen source, canonical package and reviewed 247-file inventory. Both the
inventory and archive read-back were checked; the earlier evidence archive is
referenced by hash rather than duplicated. Failed harness/checkout attempts are
labelled and retained alongside the accepted final receipts.

### Remaining publication action

The dated commit and its package are ready for owner-authorised publication.
Nothing has been published or tagged for 0.3.0. Keep the attached publication
checkout at the recorded commit while main advances with excluded notes/evidence.

1. Publish only core nbreq 0.3.0 from that exact clean checkout; verify its package
   identity before upload and bind `v0.3.0` to `ae7bfeed7cc0bffa0e5e04cf16900496a1b66f08`.
   Support-crate versions remain unchanged; SMTP and GDS deployment are outside
   this action. Re-confirm the intended date if publication is delayed.
2. Verify crates.io index/API/download identity, live versioned links and docs.rs.
   Run the published registry-only consumer matrix and audit its selected locks;
   retain publication and post-publication evidence separately.

Rust 1.85/1.86 consumers retain the documented application-lock workaround;
newer compilers retain normal dependency resolution. Migration to the 0.3 line
is explicit, and callers can opt out of individual finite deadlines with `None`.

## Related evidence

- [Timeout defaults and four-host implementation evidence](nbreq_timeout_defaults_plan.md)
- [Prior 0.2.1 release process](nbreq_021_release_plan.md)
