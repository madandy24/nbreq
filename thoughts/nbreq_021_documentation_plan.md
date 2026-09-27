# NBReq 0.2.1 documentation and examples

Opened 2026-09-27 after the accepted [0.2.1 version change](nbreq_release_api_review.md).

## Checkpoint

| Field | State |
| --- | --- |
| Working branch | `codex/nbreq-smtp`; version metadata/evidence complete at `4f7f202`, example pass at `cdc40d1` |
| Current request | Owner accepted the documentation; simplify example configuration where defaults suffice |
| Approach | Preserve the README's approved layout and GET-first introduction; explain standalone TLS, trust choices and compatibility changes in the appropriate guides |
| Current scope | Documentation accepted; redundant TLS example configuration removed and defaults/overrides documented. No production behavior changes |
| Deferred | Final package/hosted CI/publication gates |

## Work sequence

| Item | Deliverable | State |
| --- | --- | --- |
| D1 | C04 immediate TLS and C05 STARTTLS; runnable, explained and included in local example checks | Complete: existing programs clarified for 0.2.1, both included in the runner; focused Windows checks pass |
| D2 | Main README and remaining guides describe 0.2.1, trust choices and new APIs | Complete; owner accepted, defaults follow-up complete |
| D3 | Changelog/release notes include additions and reviewed behavior changes | Complete; release date deliberately pending |
| D4 | Final documentation/package link check and release candidate verification | Local documentation/link checks complete. Final clean candidate, hosted CI and published-link checks follow owner review |

## D1 decisions and checks

- C04 shows `execute_tls`; C05 negotiates a complete plaintext response before the
  consuming `into_tls` call. Keep NBReq operations visible in the example programs.
- No external server, credentials or system trust-store changes are needed. Their
  generated local CA is trusted explicitly with `SuppliedRootsOnly`; certificate
  and IP identity checks remain enabled. Explain the normal platform-trust choice
  for a public server rather than teaching private-root setup as a requirement.
- Preserve the STARTTLS boundary/ownership explanation and TLS 1.3 fixture closure
  qualification. The small text exchange is not an SMTP or IMAP implementation.
- Keep HTTP/DNS examples unchanged. The set contains 19 numbered programs: 11 HTTP,
  three DNS, and five TCP. The local runner should execute 18 cases (11 HTTP,
  five TCP, two additional HTTP controls); DNS remains explicitly opt-in.
- Run the complete local example runner after a default-feature build; also build
  and run both TLS examples with native-only features and Rust 1.85. Check formatting
  and focused strict Clippy. No broad production/remote suite rerun is needed for
  comments, documentation and runner case additions.
- A worker implements changes; independent review checks the teaching examples,
  release-runner integration and actual validation evidence before completion.

Evidence lab: `C:/User/projects/nbreq/target/examples-021-20260927/`.

## D1 result

C04/C05 already supplied the requested local demonstrations. This pass keeps their
client logic and shared TLS fixture intact, adds concise explanations of the direct
and consuming-upgrade calls and trust configuration, and updates the example index
and TLS guide introduction for 0.2.1. CI and packaged-consumer checks now execute
both programs and require their distinct protected-echo success messages. Tool
documentation and the registry runner agree on 19 programs and 18 local cases.

Windows x86_64 validation passed on stable Rust 1.97.1 and minimum Rust 1.85.0:

| Check | Result |
| --- | --- |
| Default-feature build | All 19 example programs built |
| Local example runner | All 18 cases passed, including exact protected replies from C04/C05 |
| Native-only TLS examples | Both built and ran successfully |
| Rust 1.85 TLS examples | Both built and ran successfully |
| Focused strict Clippy / workspace formatting | Passed |
| Source integrity | Example, manifest, lock and runner hashes unchanged across validation |

These checks execute local verified TLS handshakes and application exchanges; they
are not live external-network, remote-platform or final packaged-release evidence.
DNS examples were built but not run against the host's live DNS configuration.
Independent review accepted the code/documentation diff and raw validation evidence
with no remaining findings. No production, dependency or fixture behavior changed.

Retained [evidence](evidence/nbreq-examples-021-20260927.tar.gz): 80 regular members,
52,256 bytes, SHA256
`3bcf1721a3df4fad3f2c0b2554e5deab455be0bbf0f38a8e417883eba88e0463`.
The [artifact index](evidence/nbreq_examples_021_artifacts.json) records member and
executed-binary hashes. Commands, raw logs, example sources and the scoped diff are
included; build outputs and binaries are excluded. D2/D3 are next; rebuild the final
release candidate after documentation is complete.

## D2/D3 scope and validation

- Describe the intended 0.2.1 release without claiming publication or inventing its
  release date. New-API dependency examples require at least `"0.2.1"`.
- Keep Highlights above the convenience GET, short cancellation/DNS/TCP snippets,
  and curl history brief and last. Link to the runnable TLS examples.
- Explain platform, supplied-only and optional Mozilla trust; feature enablement
  does not select a policy. Document root refresh/rebuild requirements and the
  absence of automatic verification fallback or portable OS revocation policy.
- Correct hostname TCP's feature requirements and qualify actual Wine evidence.
  Keep GDS integration and the unpublished SMTP crate's release separate.
- Disclose the behavior changes already accepted in the API review, including
  supplied-root parsing, unsupported trust configurations, TCP retained capacity,
  peer-FIN timeout handling and the TLS final-shutdown correction.
- Validate included-guide doctests, exact standalone README/TLS snippets and local
  documentation link destinations/anchors. Compile relevant snippets on stable
  and Rust 1.85. Follow the repository's required verifier before committing.
- Independent review checks API accuracy, trust/feature claims, migration advice
  and evidence; the owner then reviews the prose before release preparation.

Documentation evidence lab: `C:/User/projects/nbreq/target/docs-021-20260927/`.

## D2/D3 result and handoff

The README keeps Highlights above its convenience GET, existing short snippets and
brief final History section. It now presents direct TLS, consuming upgrades and
explicit trust selection. The guides include supplied-only/bundled-root examples,
correct native-only hostname support, TLS boundaries and qualified Wine evidence.
Migration notes and the 0.2.1 changelog disclose the accepted compatibility details.
Security and contributor guidance match current trust choices and repository gates.
SMTP remains a separate unpublished crate; no combined release is promised.

Independent review corrected one timeout comment: for immediate TLS the earlier
connect/handshake deadline bounds complete DNS/TCP/TLS establishment. Request,
getter and builder descriptions now agree. No executable Rust, dependency, test
fixture or runtime behavior changed.

| Validation | Result |
| --- | --- |
| Existing full offline verifier, Windows x86_64 stable 1.97.1 | All 30 stages passed in 311.179 seconds |
| Exact README snippets | All four compile on stable and Rust 1.85 with only their documented Engine/Duration context supplied |
| Standalone TLS guide | All three no-run snippets compile on stable, Rust 1.85 and native-only |
| Included guides and SMTP README | 34 core and two SMTP doctests pass on Rust 1.85; stable covered by the full gate |
| Final API comment correction | Only TCP doc comments differ from the full-gate snapshot; executable lines identical. Final stable/MSRV 34 core doctests, strict root/SMTP rustdoc and formatting pass |
| Markdown/package checks | 55 link destinations/anchors resolve against the working package inventory or, for SMTP/contributor material, repository files |
| Generated API documentation | 586 links across root/SMTP indexes and affected TCP API pages resolve locally |
| Independent review | Final prose, API contracts, raw logs and source integrity accepted; no remaining findings |

The initial package-list attempt rejected the uncommitted tree; the explicit
`--allow-dirty --list` retry only inventories reviewed files and is not clean
candidate verification. The generated-link checker was corrected to recognize
rustdoc's literal percent-encoded IDs as well as decoded fragments; no document or
Cargo check changed for that validator correction. Both details remain in the logs.

Cross-file links in rustdoc-included guides use the intended `v0.2.1` source URLs.
Their targets are verified locally; they are not live hosted-link evidence before
the release tag is published. Ordinary README/standalone-guide links remain relative.
No external network snippets were executed, and no new remote-platform pass is
claimed for this documentation slice.

Retained [documentation evidence](evidence/nbreq-docs-021-20260927.tar.gz): 84 regular
members, 269,121 bytes, SHA256
`3ba1f453d90c3dd5fcdc8c0496377f89c0af4f82d45e57609740ef997820c497`.
The [artifact index](evidence/nbreq_docs_021_artifacts.json) binds raw logs, source
snapshots, exact snippet scaffolds and local link inventories. Build outputs and
binaries are excluded.

The owner accepted the prose and requested the defaults follow-up below. Next,
rebuild the clean release candidate and complete hosted/registry gates. No push,
merge, publication or GDS change is part of this documentation pass.

## Owner follow-up: simplify examples where defaults suffice

The owner accepted the draft and requested less timeout/queue configuration in the
README and TLS guide when the existing defaults are suitable. Source inspection
found 256 KiB default TCP windows per direction and a ten-second TLS establishment
deadline. HTTP request timeouts, DNS total timeout, plain TCP connect timeout and
connected read/write inactivity timeouts are unset unless explicitly selected.

Removed redundant TLS establishment and queue choices from the immediate-TLS
snippet, plus its unused write-inactivity setting. Kept the greeting-read deadline
and the README's finite HTTP/DNS/plain TCP deadlines: those do not have equivalent
finite defaults. Added a defaults/override table to getting-started and links from
the README and TLS guide; retained detailed configuration in the other examples.
Runtime defaults remain unchanged. Independent review confirmed the values and
retained timeout choices; its only correction clarified that the shared queue
budget covers both HTTP upload and response streaming windows.

Validation on Windows x64: all 30 verifier stages passed (259.687 seconds), exact
README snippets compiled on stable and Rust 1.85, TLS snippets compiled on both
toolchains and native-only features, 34 core and two SMTP MSRV doctests passed,
and 61 local Markdown/package link checks passed. The input hashes stayed fixed
through the full verifier. Afterwards the sole change was removing `response`
from the shared-budget sentence; byte comparison confirmed that exact prose delta,
unchanged code blocks and other captured inputs. Final strict rustdoc passed.
Network snippets were compiled, not executed against remote hosts, and future
v0.2.1 links were checked against local targets rather than an unpublished tag.

Evidence lab: `C:/User/projects/nbreq/target/docs-defaults-021-20260927/`. Retained
logs, hashes and snapshots are indexed in
[nbreq_docs_defaults_021_artifacts.json](evidence/nbreq_docs_defaults_021_artifacts.json),
with build outputs excluded from the companion archive. No runtime changes,
remote-host testing, push or publication were needed for this follow-up.
