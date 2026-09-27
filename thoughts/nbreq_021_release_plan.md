# NBReq 0.2.1 final release

Opened 2026-09-27 after the owner accepted the documentation and authorized
housekeeping, main integration, final candidate checks, hosted CI and publication
after passing gates. Core only; helper versions remain unchanged and SMTP
publication/GDS integration stay separate.

## Resume checkpoint

| Item | State |
| --- | --- |
| Baseline | Local and remote main match published 0.2.0 completion commit `6ada116`; main is clean |
| Reviewed work | `codex/nbreq-smtp` through `e025c1d`; only two future-timeout note edits remain |
| Existing evidence | Documentation/defaults pass: full 30-step Windows verifier, stable/MSRV snippets, strict rustdoc and independent review; runtime/platform/Wine evidence remains in the linked plans |
| Current stage | Commit notes and this tracker; fast-forward main, then freeze an exact candidate |
| Publication | Not started; v0.2.1 tag absent at initial remote check |
| Local evidence | `target/release-021-20260927/` (commands, package hashes, consumer results and hosted receipts) |

## Gates

| Gate | Required result | State |
| --- | --- | --- |
| R1 Integration | Clean main containing the reviewed branch and accepted notes; no unrelated edits | Pending |
| R2 Candidate | Exact clean package inventory, links, registry helper identities; packaged TLS regressions and bundle-on/off consumers | Pending |
| R3 Hosted acceptance | Eight platform/stable/MSRV verifier jobs plus advisory/license checks; candidate registry-helper consumer matrix | Pending |
| R4 Publication | Final dated changelog, exact dry-run/archive identity, publish core 0.2.1, matching tag | Pending |
| R5 Published acceptance | Registry API/index/download checksums, registry-only matrix, live README/docs.rs/versioned-link checks | Pending |
| R6 Closeout | Retained evidence, updated checkpoint and clean pushed main | Pending |

Do not treat old package hashes as current-candidate evidence. Preserve failed
attempts and distinguish cached/offline, registry-helper candidate and fully
published-registry results. If the final date or another packaged file changes,
refresh the candidate identity and verify the exact delta before publication.

## Decisions and scope

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

- [Documentation and examples](nbreq_021_documentation_plan.md)
- [API/version decision](nbreq_release_api_review.md)
- [Portable TLS and platform proof](nbreq_portable_tls_plan.md)
- [HTTP timing-test repair](nbreq_http_timeout_test_plan.md)
- [Previous publication procedure](nbreq_020_publication.md)
