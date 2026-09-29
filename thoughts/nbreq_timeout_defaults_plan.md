# NBReq 0.3.0: sensible timeout defaults

Opened 2026-09-29. The owner requested sensible defaults and explicitly accepted
0.3.0 as a behavioural compatibility boundary. A healthy, idle, established
TCP/TLS connection must remain open; establishment must have a deadline.
Explicit `None` must restore the former unbounded HTTP/DNS/TCP timer behaviour.

## Resume checkpoint

| Item | State |
| --- | --- |
| Baseline | Clean main `b0e8f1910234fbce9b99e568c93eed12bdc8359f`; 0.2.1 already published |
| Active stage | Implementation committed as ca08014; owner accepted README/example follow-up; [0.3.0 release gates](nbreq_030_release_plan.md) in progress |
| Team | Sol implementation and tests; independent Astra review/fix/re-review; root owns documentation, versions, source freezes and bridge |
| Evidence lab | `target/timeout-defaults-20260929/`; retain raw failed and passing results |
| Priority host | Scaleway Apple Silicon, Intel Mac and Linode bridges verified; prioritise Scaleway before retirement |
| Release scope | Implement and verify 0.3.0; publication remains a separate release action |

## Agreed policy

| Timer | Default |
| --- | --- |
| Buffered HTTP/HTTPS connect | 10 seconds |
| HTTP/HTTPS useful-progress inactivity | 30 seconds |
| Buffered HTTP total | 120 seconds |
| Fresh streaming HTTP builders | Connect/inactivity as above; no total lifetime cap |
| Public DNS total | 30 seconds |
| Plain TCP establishment | 10 seconds |
| Connected TCP/TLS read inactivity | Disabled |
| Connected TCP/TLS write inactivity | 30 seconds, only while accepted output awaits progress |
| Standalone TLS establishment/upgrade | Existing finite 10-second deadline retained |

Existing HTTP/DNS/TCP fluent setters accept `Duration`, `Some(Duration)` and
`None`. Omitting a setting chooses its default; explicit `None` disables that
timer. `RequestOptions::default()` uses buffered HTTP defaults, while explicitly
assigned `None` fields disable timers. No implicit Engine inheritance, sentinel
zero, default-value detection, or new policy hierarchy.

Fresh `StreamRequest` builders clear only the initial total timeout. Converting
an existing `Request` to a `StreamRequest` preserves all options, including the
buffered request's total timeout. Supplying a complete options object replaces
the complete options object; do not silently reinterpret it at submission.

Deadlines retain existing admission-based clock origins. HTTP redirects retain
the original total budget; per-hop connect/inactivity clocks follow existing
semantics. Read backpressure pauses the relevant inactivity timer; useful I/O
progress resets it. No overall lifetime deadline is added to established TCP/TLS.
Manual Engines still require the owner to drive them.

Direct TLS uses the earlier of the TCP connect and TLS establishment deadlines.
Increasing TLS's deadline alone is insufficient if TCP still has its new
10-second default: raise or disable TCP's connect timer too. STARTTLS begins
its existing fresh TLS upgrade budget. The already-finite TLS handshake policy
is retained; `None` support restores timers that were previously optional.

## Work and gates

| Stage | Required result | State |
| --- | --- | --- |
| D0 Contract/red | Public defaults, explicit values/None and idle-vs-stalled semantics; preserve failing proof | Four expected runtime reds on 0.2.1, two controls passed; raw source/log preserved |
| D1 Implementation | Minimal default/setter changes; existing expiry machinery retained; reviewer findings resolved | Accepted five-file implementation; eight focused tests green |
| D2 Integration | Core 0.3.0, local SMTP/consumer requirements, package/tool references, migration guide and examples aligned | Implemented and reviewed; helper versions/runtime dependency ranges unchanged |
| D3 Local verification | Focused green, full required verifier, Rust 1.85 relevant checks and consumer API proof | Windows stable/MSRV: 30 stages and 18 local example executions each; consumer 44/44 steps, new API test 20/20; package listing includes migration guide |
| D4 Platforms | Audited source freeze, actual remote execution, exact compiler/source receipts | Windows, Linux, Intel Mac and Apple Silicon: stable/MSRV passed, hashes unchanged; all raw evidence local |
| D5 Closeout | Independent raw-evidence review, retained evidence, updated plan and reviewed commit | Complete in the commit containing this plan: source, docs, all results and the retained archive accepted |

Do not run competing local Rust builds. Tests should observe the contract and
meaningful timer behaviour, use existing deterministic/controlled fixtures where
possible, and avoid host-speed assumptions. Preserve initial failures rather
than overwriting logs. Distinguish actual remote execution from synthetic tests.

Queue sizes, body budgets, concurrency profiles, DNS server/retry policy,
certificate trust, GDS integration and SMTP protocol policy are unchanged.
SMTP's local dependency must follow core 0.3.0 so the workspace builds; SMTP
publication is not part of this pass.

## Decisions, findings and evidence

- Owner clarification: quiet established TCP is valid; connecting TCP is bounded.
- Owner accepted 0.3.0 and requested explicit `None` opt-outs.
- Review identified option-preserving Request-to-Stream conversion and the
  combined TCP/TLS establishment deadline as documentation/regression requirements.
- Scaleway read-only preflight request: `20260929-081737-3b7e21f6`.
- Intel preflight `20260929-082449-923f4071` and Linode preflight/follow-up
  `20260929-082453-e1609666` / `20260929-082823-e571fd59` confirm the intended
  hosts and stable/Rust 1.85 tools. Linode needs the explicit Cargo bin path.
- Owner authorised old NBReq cleanup. After a read-only size/process check,
  request `20260929-083232-18723855` removed only the inactive build subdirectory
  of the old TLS gate, preserving logs and receipts; free space rose to 8.7 GB.
- The first 167-member source archive (`04160b843210` manifest) is retained with
  its failed Windows verifier. No upload of that superseded archive occurred.
  Its failure is test lint, not a runtime timer failure. Review also requested
  a SIGTERM handler in the remote harness so interruption cleans up its owned
  Cargo process group; that correction precedes the next source freeze.
- Corrected snapshot manifest: `af66faa1fa91b2ea096fca5b02b591c3b106129837695be151101a9b9f7e8fe9`;
  archive SHA256 `09e8ca9307d1e743b801b20eec752c7bba6972777b7eb9984a481b0317ce182c`.
  Independent review verified all 167 members and the limited lint/harness delta.
  Uploads succeeded on all three authorised hosts.
- A second local attempt reused a cached xtask binary containing the previous
  snapshot's compile-time workspace path. Its log explicitly identifies that old
  root; this is not evidence for the corrected source. Retained the failed attempt
  and restarted with an isolated target directory. Remote targets are isolated too.
- macOS checksum input requires the standard two-space separator; initial launch
  attempts stopped at checksum parsing before extraction. Corrected and retried;
  the Linux checksum and launch succeeded on the first attempt.
- Windows isolated gate (`windows-af66faa1fa91-isolated`) passed on Rust 1.97.1
  stable and 1.85.0, with source hashes unchanged. Apple Silicon completed both
  toolchains and its 107,923-byte evidence archive was retrieved and hash-checked
  via bridge request `20260929-085100-88c4b6c5`; raw logs are safely local.
- Review accepted the final source and documentation. Existing controlled runtime
  tests cover deadline expiry, progress, backpressure and TLS; new tests assert
  defaults and optional setter behaviour without waiting literal 10/30/120 seconds.
- The independent Windows consumer matrix passed all 44 steps across four cases
  (two compilers, each with fresh and Mio-coexistence dependencies). Five feature
  modes per case exercised the new optional-timeout API test: 20/20 passed.
  Stable used fresh locks; Rust 1.85 used the already documented explicit
  compatibility selection, retaining both original and selected locks. These are
  local source/helper overrides, not registry-only acceptance of unpublished 0.3.0.
- Offline package listing succeeded and contains `docs/migrating-to-0.3.md`.
  Its first post-command assertion used POSIX separators on Windows output;
  normalising separators corrected the assertion without rerunning Cargo.
- Intel Mac stable/MSRV passed and its evidence was retrieved via
  `20260929-085543-d02b10b7` (107,706 bytes). All accepted platform runs use isolated
  build output and report unchanged source hashes.
- Linux stable/MSRV passed and evidence was retrieved via
  `20260929-090302-56e3dae8` (109,047 bytes). Its longer runtime did not produce a
  functional or timing failure. All eight platform/compiler combinations completed
  the 30-stage verifier plus 18 local example executions: 240 stages and 144 example
  executions in total. Live DNS/HTTPS examples and Wine were not rerun by this pass.

| Platform | Stable rustc | MSRV rustc | Full verifier | Local examples |
| --- | --- | --- | --- | --- |
| Windows x64 | 1.97.1 | 1.85.0 | 30/30 on each | 18/18 on each |
| Linux x64 | 1.98.0 | 1.85.0 | 30/30 on each | 18/18 on each |
| Intel macOS | 1.98.0 | 1.85.0 | 30/30 on each | 18/18 on each |
| Apple Silicon macOS | 1.98.0 | 1.85.0 | 30/30 on each | 18/18 on each |

## Retained evidence

[Evidence archive](evidence/nbreq-timeout-defaults-030-20260929.tar.gz): 1,768,435
bytes, 435 files; SHA256
`bd092c6c093c3023fd31b2a06d9a15f205a064603a9e2e3d28984160f50f9a47`.
It includes both source snapshots, baseline red tests, rejected/failed attempts,
all accepted host logs, consumer source/locks/results, package listing, harnesses,
independent review and per-file inventory hashes. Build directories and raw
private bridge payloads are excluded. The working source was checked byte-for-byte
against all 166 frozen inputs before retention.

Final independent audit confirmed all 435 archive members and 434 indexed input
hashes, both nested source snapshots, the summaries against raw results, and the
absence of build output, credentials or private bridge payloads. No review blocker
remains. The commit containing this checkpoint concludes implementation; it does
not claim publication or registry-only acceptance.

## README and examples follow-up, 2026-09-29

After implementation commit `ca08014`, the owner requested a simpler README and
commented example settings at their actual default values. README snippets now
omit timeout setters. All 19 runnable examples use the library's request defaults
and show optional timeout lines marked `Default setting`, with fully qualified
`std::time::Duration` so uncommenting needs no extra import. Streaming HTTP shows
no total cap; TCP reads show `None`. Local cancellation/callback waits, event-loop
budgets and fixture-server deadlines stay active and are distinguished from
library request policy. The two guides' example cross-references were aligned.

Validation is in `target/example-defaults-20260929/`: format, example lint/build,
18 local example executions, native-only and Rust 1.85 example compilation,
doctests and scratch compilation of all 53 commented timeout settings enabled.
All checks passed: 19 examples build, all 18 local executions pass, and all 53
commented settings compile when enabled together in scratch copies. Independent
review accepted the source, documentation and all eight verification results,
including 38 doctests, after correcting a stale guide cross-reference. This follow-up
was accepted by the owner for integration and pre-release checks; no new runtime
source changes, remote test pass or release claim is implied. The earlier full
platform matrix continues to describe implementation commit `ca08014`.

## Release follow-up

This implementation pass does not publish 0.3.0. The owner authorised integration,
push, CI and package checks, tracked in the [0.3.0 release plan](nbreq_030_release_plan.md).
Publication/tagging still requires a separate decision. Current guides now use
neutral 0.3.0 wording and immutable v0.3.0 links; their paths are checked locally
before that tag exists. The changelog retains an undated unreleased heading until
publication is authorised. No support-crate version change is required by this work.

Applications on `nbreq = "0.2"` remain on that compatible line. Adoption of 0.3
is deliberate and should review the migration guide, especially long polling,
quiet HTTP streams, custom TLS establishment budgets and explicit `None` opt-outs.

## Related work

- [Deferred defaults note](nbreq_021_documentation_plan.md#future-revision-sensible-timeout-defaults-throughout)
- [0.2.1 release and evidence](nbreq_021_release_plan.md)
- [TLS architecture and prior host evidence](nbreq_tcp_tls_plan.md)
