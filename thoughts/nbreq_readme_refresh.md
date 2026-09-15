# R5 README refresh

2026-09-15. Implemented in R5, committed as `655a6e3` and integrated into pushed main `451a769`. The owner requested a consumer
introduction led by convenience GET, short cancellation/DNS/TCP snippets and guide/example links,
with curl history brief and last.

## Result

- Introduces HTTP/DNS/TCP and the choice of blocking, background or manually driven work.
- Leads with the default dependency and `Engine::get(...).call()`. Four readable snippets omit
  full-program boilerplate and rustdoc-only helper lines; surrounding prose explains their context.
- Highlights explicit ownership/shutdown, cancellation, connection reuse, resource controls and
  verified HTTPS. Cancellation observes the canonical completion and acknowledges the race.
- Links to the getting-started guide, 17-program example index, migration notes and focused examples.
- Replaces backend internals and historical test narration with a short scope/configuration section.
  Retains HTTP/1.1/cleartext-TCP scope, macOS DNS limits, shutdown caveat and dependency policy.
- Curl appears once, in the two-sentence History section at the end. Project attribution, dual
  license and contribution terms remain. At the owner's follow-up request, the original eight
  Highlights bullets are restored above the GET section and both READMEs describe release 0.2.0.
  Apart from that insertion and version paragraph, the approved rewrite is unchanged.

## Validation and remaining release work

All four exact Rust code blocks compile in a temporary consumer on Windows x64 with stable Rust
and Rust 1.85. The main and R5 README code blocks are identical. All 17 relative link destinations
are present in Cargo's actual package inventory, and referenced Markdown heading anchors resolve.
No runtime, manifest, dependency, or example-program changes belong to this README slice.

The [evidence record](evidence/nbreq_readme_refresh_20260915.json) records commands and source hashes;
local logs and original README backups are under `target/readme-refresh-checks` in main.
The initial inventory comparison needed Windows path-separator normalization; no package file
was missing. These are compilation/link checks, not a repetition of the completed network tests.

README and example presentation, main integration/push and the final clean package freeze are
complete. The [housekeeping checkpoint](nbreq_housekeeping.md) records explicit public-upload
approval, source/package identities and updated hosted CI. The subsequent [publication pass](nbreq_020_publication.md)
completed helper/root publication, registry-only checks and actual public README/docs inspection.
This README slice itself made no GDS changes or crate publication.

## Original main reconciliation check — subsequently completed by housekeeping

A read-only comparison on 2026-09-15 found main is an ancestor of the R5 branch, which contains
18 additional commits. Main has substantial uncommitted/untracked work, mostly copies of completed
release work. Library source, tests, support crates, root manifest/lockfile and the new examples
match the R5 working tree after normalizing line endings. The new examples and README are still
uncommitted in R5 as well.

Reconcile the remaining documentation/evidence and F5-tool differences before integrating:
preserve main's buffered-budget benchmark knobs/gauges and local ignore entry, and carry over
R5's comparison runner, fixture improvements and quiescence check. The benchmark lockfiles also
differ. This is release-state reconciliation, with no new library defect identified by the
comparison. No files in that remaining work were overwritten by this README request.
