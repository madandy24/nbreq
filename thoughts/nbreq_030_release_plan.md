# NBReq 0.3.0 pre-release checks

Opened 2026-09-29 after the owner accepted the README/example simplification and
requested standard tests, CI and processes prior to release. Commit/push and
candidate CI are authorised. Publication and release-tag creation are not part
of this request; return the concrete candidate and any remaining decision first.

## Resume checkpoint

| Item | State |
| --- | --- |
| Baseline | Local main ca08014; remote main b0e8f19 at preflight |
| Scope | 0.3.0 timeout defaults plus accepted documentation/examples and any demonstrated test correction; no new runtime work |
| Current stage | Full local verifier passed; integrate reviewed source, then exact package and hosted checks |
| Source | Approved example edits plus neutral version prose, versioned guide links and package checks |
| Local lab | target/release-030-20260929; preserve failed attempts and exact candidate identities |
| Publication | Held; no publish or tag action authorised by this pass |

## Gates

| Gate | Required result | State |
| --- | --- | --- |
| P1 Local/integration | Full 30-stage verifier, clean reviewed commit, pushed main | Local 30/30 passed; commit/push next |
| P2 Candidate | Exact package inventory/links, registry helpers, unpacked timeout/TLS regressions, independent stable/MSRV consumers, dry run | Pending |
| P3 Hosted CI | Eight platform/compiler verifier jobs plus advisory and license jobs | Pending |
| P4 Hosted candidate | Eight platform/compiler candidate jobs using published helpers; retained logs, locks and artifact digests | Pending |
| P5 Review/closeout | Independent acceptance, retained evidence, ready-to-publish checkpoint and remaining decisions | Pending |

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
- CHANGELOG remains explicitly undated/unreleased. Adding a release date changes
  package bytes: refresh the candidate, inspect the exact delta and repeat the
  necessary final checks before publishing. Never reuse an old archive hash.
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
  exception is unchanged; fresh consumer and example locks still need their scans.
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

## Related evidence

- [Timeout defaults and four-host implementation evidence](nbreq_timeout_defaults_plan.md)
- [Prior 0.2.1 release process](nbreq_021_release_plan.md)
