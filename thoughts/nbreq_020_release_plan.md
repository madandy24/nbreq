# NBReq 0.2.0 release checklist

Opened 2026-09-10; updated 2026-09-14. Status: R1 verified; R2 platform proof verified with local
support overrides; R3 has tested Darwin and winpoll candidates. The clean root checkpoint is
committed and packaged. Final reliability, exact-candidate CI and registry-only gates remain.
W-01 is reproduced and fixed in `149450d`; actual Wine 5 x86 and native Windows gates pass.
On 2026-09-14 the owner reports a live GDS pilot under Wine has run this code for several days
with no problems observed. This adds application evidence; it does not specify an exact DLL
hash/Wine version, workload or shutdown check. The [investigation](nbreq_wine_dns.md) retains
those provenance limits. The library path can proceed; GDS's application/device profile is not a prerequisite for
releasing nbreq's general-purpose library. Consumer documentation/examples and external checks
are authorized; publication remains separate. This checklist joins the [follow-up programme](project_nbreq_followup_plan.html)
and [memory work](nbreq_memory_plan.md); it does not replace their historical evidence.

## Resume checkpoint

| Field | State |
| --- | --- |
| Candidate | Root 0.2.0, Rust 1.85 / edition 2024. Current fix checkpoint `149450d6543376cdb8255b55d7eb286e98207f42` on `codex/wine-dns-adapters` extends `7693d6c` with bounded Windows DNS discovery and winpoll 0.1.1. Earlier Darwin `4a6c807` and root branches are preserved. [Wine report](nbreq_wine_dns.md) and [clean candidates](nbreq_020_clean_candidates.md) identify the packages. The scoped fix is mirrored to main; unrelated F5 work remains separately owned. |
| Verified core | F1/F2 Resolver and TCP, F3 private DNS codec, F4 feature boundary, F6 bounded macOS support, F7 HTTP convenience, review fixes and memory M0–M3 have accepted evidence. |
| Source recheck | Before R1, all 119 ordinary inputs in the final M3 E manifest matched on 2026-09-10. R1 now changes docs, example registration/examples and source doc comments/includes; it does not change runtime implementation. The new release rehearsal freezes its own manifest. |
| Registry recheck | Rechecked 2026-09-14 through the crates.io APIs: nbreq latest 0.1.1, nbreq-winpoll latest 0.1.0; nbreq-darwin returns 404. Root 0.2.0 and the two required support releases are not published. |
| Package inventory | Latest clean root and winpoll archives contain 82 and 10 files with commit `149450d` and no dirty flag. Both verify for Windows x86; root uses explicit local Darwin and winpoll overrides. Winpoll's exact archive passes 13 tests on Windows x64/Rust 1.85. Prior Darwin archive has 8 files. Thoughts, GDS evidence and private tools are excluded from root. Registry-only resolution remains open. |
| Active slice | W-01 confirmed: Wine 5 returns unaligned descriptive strings/records and unchanged ipconfig reproduces the exact panic. New byte parser and policy regressions pass, including actual Wine default/native-only lifecycle probes. Final Windows verifier 24/24 (81.561 s). Earlier deadline-sensitive failures and an unbounded test fixture are retained with qualified interpretation in the report. |
| Evidence | Earlier R1/R2 [manifest](evidence/nbreq_020_artifacts.json) and [archive](evidence/nbreq-020-consumer-20260910.tar.gz) remain intact. New [candidate manifest](evidence/nbreq_020_candidate_artifacts.json) and [91-file archive](evidence/nbreq-020-candidates-20260910.tar.gz), SHA256 `62b8aac61a4bb1bcfb48410c9b6c82b5a7f4ca8c4335cca86375a67a4aff6986`, preserve clean packages, failures, proof and passing follow-ups. |
| Next slice | R4's four-host four-hour observations and Windows x86 companion have passed; finish the scoped Linux 0.1.1 comparison and evidence handoff, then R5's final candidate checks. Then separately authorize/publish Darwin 0.1.0 and winpoll 0.1.1, prove registry-only root installation, and publish/smoke 0.2.0. The successful owner-reported GDS pilot supports W-01; collecting its exact provenance does not block these library checks. |
| GDS boundary | Preserve its source and installed DLL. M4 device/application memory acceptance waits for GDS availability. Do not export GDS policy as nbreq defaults. |

## Remaining work

Status is deliberately split between existing evidence and final release acceptance.

| ID | Status | Work and exit condition |
| --- | --- | --- |
| R1 — Consumer contract and documentation | Verified on Windows, Linux and both Macs | Scoped API audit, migration notes, 0.2 snippets, ownership/budget/error guidance, DNS/TCP walkthroughs, platform/feature matrix and Darwin limitations are in place. Seven examples are registered and packaged. The isolated root checkpoint removes unreleased wording; main retains it pending integration. |
| R2 — Independent consumer proof | Platform proof verified with explicit Darwin override; registry gate open | External temporary workspaces on all four hosts exercise ordinary default/native-only/minimal/test-support APIs against normalized archives. Stable and Rust 1.85 pass fresh online dependency resolution, HTTP/callback/manual/cancel/shutdown, streaming, TCP, retained budgets and feature probes. The same three HTTP tests also pass against registry 0.1.1. All seven packaged examples run on each host, including live DNS/HTTPS. The four independently resolved locks are identical. R3 publication is still needed for registry-only acceptance. |
| R3 — Support crates and package graph | Darwin and winpoll candidates tested; publication remains open | Darwin `4a6c807` and its tested 8-file archive are unchanged. Winpoll 0.1.1 from `149450d` adds bounded DNS discovery; its clean 10-file archive verifies on x86 and passes 13 tests on x64/MSRV. Root removes ipconfig/widestring; advisory and generated-license checks were refreshed. Publish both helpers only after separate authorization, then prove registry-only root resolution. |
| R4 — Final reliability observation | Four-host soaks and x86 companion passed; Linux common-path comparison in progress | All four full verifiers, four-hour default mixed observations, harness checks and default/native-only rehearsals passed; the complete Windows x86 preflight passed too. Independently verified archives retain every cycle, source/binary hashes and clean shutdown evidence. Windows 0.1.1 comparison and longer noisy-small-body follow-up are complete; Linux is building the same frozen inputs after explicit source-upload approval. [Checkpoint and results](nbreq_r4_reliability.md) retain exact evidence and limits. Fixture reds/greens and harness are committed; runtime implementation is unchanged from the Wine fix. |
| R5 — Exact release candidate | Clean W-01 fix checkpoint; acceptance open | `149450d` and its root/winpoll packages are clean; Windows/Wine proof is in [W-01](nbreq_wine_dns.md). Earlier `7693d6c` archive-consumer checks remain scoped historical results. Final matrix/hosted CI, applicable x86 companions, final advisory/license gates, registry-only consumer proof and docs.rs/package-link checks remain. Track the deadline-sensitive test failures and unbounded fixture separately; no universal test-authority claim. |
| R6 — Root publication and registry smoke | Separate owner action after R1–R5 | After R3's authorized support publication and the registry-resolved root proof, publish nbreq 0.2.0. Inspect the published metadata/docs and run a clean consumer using only the registry release. Publication is not implied by this checklist. |

R3's support publication necessarily precedes the final registry-only root rehearsal in R2/R5.
Before that point, a clearly labelled local support override can validate code, but cannot close
the registry-consumer gate. Dependency ranges may select different versions in a fresh consumer
than in our Cargo.lock; the advertised MSRV must be checked against that distinction.

R4 is now authorized and underway; [working checkpoint](nbreq_r4_reliability.md) records fixture
proof, historical-failure investigation and the four-hour/four-host soak contract.

## 2026-09-14 release-path review

The clean fix worktree remains at `149450d6543376cdb8255b55d7eb286e98207f42`. A read-only
comparison finds no differences in its source, tests or support-crate files versus main
(normalizing line endings). Main still contains separately owned uncommitted tooling and
documentation; use the isolated candidate when freezing release inputs.

Recommended order:

1. Finish R4: reconcile F5 observation work with accepted M1–M3 evidence, address the unbounded
   fixture cleanup and investigate recurring timing failures, then run the planned multi-hour
   HTTP/Resolver/TCP soak with pressure, retained bodies, cancellation and joined shutdown.
2. Finish pre-publication R5 on the final source: Windows/Linux/Intel Mac/Apple Silicon matrix,
   supported stable/MSRV and x86 checks, hosted CI, refreshed advisory/license checks and package
   documentation checks. Reuse prior evidence as history; identify new results by exact source.
3. Publish the prepared nbreq-darwin 0.1.0 and nbreq-winpoll 0.1.1 after separate owner authorization.
4. Complete R2/R5 using registry support dependencies without local overrides, including a fresh
   consumer/MSRV graph. Then publish nbreq 0.2.0 under R6 and verify the registry release/docs.

No new feature package or additional GDS memory policy is proposed. The common 0.1.1 comparison
is a regression check where evidence is missing, not an open-ended optimization programme.
Actual 128/256 MB device profiles remain separate. This review changed documentation only;
it did not rerun tests, start remote jobs or publish anything.

## W-01 — fixed library boundary; live GDS pilot reported successful

[Investigation and GDS handoff](nbreq_wine_dns.md) records the exact reproduction on Wine 5.0
(Ubuntu 5.0-3ubuntu1), x86 Windows debug binaries. Odd in-buffer UTF-16 pointers trigger unchanged
ipconfig/widestring. Fix `149450d` avoids unused strings and validates required fields through
byte slices, retaining selection/suffix policy and scopes. Actual Wine and native Windows checks
pass; original failures and test-oracle limitations are preserved. The deployed GDS DLL hash and
application-prefix provenance remain unrecorded here. The owner reports several successful pilot
days as of 2026-09-14; no separate shutdown or scan-test transcript was supplied. The Delphi
backend-selection fix belongs to GDS.

The [W-01 artifact manifest](evidence/nbreq_wine_dns_artifacts.json) and
[archive](evidence/nbreq-wine-dns-20260910.tar.gz) preserve source, packages, binaries and logs.

## Documentation findings addressed by R1

- Dependency snippets now select 0.2 with an explicit unreleased-candidate notice.
- The feature/platform matrix states the accepted Windows/Linux/macOS scope and rejects an
  implied promise of every macOS split/scoped DNS setup.
- Public Resolver and standalone TCP now have worked walkthroughs and runnable examples.
- SECURITY.md includes the Darwin unsafe support boundary and publication prerequisite.
- Migration notes collect changed ownership semantics, added APIs, generic `drive_until`,
  configuration inheritance, platform constraints and the pressure-error retry caveat.
- `autoexamples = false` had left only one example registered. All seven shipped examples are
  now declared with their required features; new docs/examples are included in the package.

## Memory guidance for general consumers

Keep nbreq's existing 16 MiB per-body defaults and opt-in aggregate cap unless a separate library
policy decision changes them. GDS's 24 MiB body ceilings, 64 accepted-request setting, WebRPC
queue watermarks and scheduling rules belong to GDS.

Explain how to combine concurrent-work admission, per-operation limits and retained-capacity
budgeting. Distinguish buffered-body accounting from streaming queues, TCP reservations and
whole-process RAM. Shared responses retain one charge until the last owner drops; unique Vec
transfer moves responsibility to the application. A pressure failure after a POST does not
establish that replay is safe. Illustrative small-workload settings need a clearly stated workload
and headroom; they must not imply acceptance on an unmeasured 128/256 MB device.

## Evidence and boundaries

The latest broad correctness checkpoint is the [R1/R2 consumer rehearsal](nbreq_020_consumer_readiness.md):
seven full 24-step verifiers plus independent packaged consumers on all four hosts. [M3 E](nbreq_m3_memory_controls.md)
retains the accepted Windows x86 companions and paired Windows/Linux memory/performance observations.
F6 also records earlier physical Intel and Apple Silicon lifecycle/soak evidence. Those results
are useful foundations; they are not a final mixed-capability soak of the post-M3 release candidate.

CI already defines stable Windows, stable/MSRV Linux and stable/MSRV Intel/Apple Silicon macOS,
plus advisory and generated-license jobs. The work is to obtain passing final-candidate results,
not to invent a new CI matrix. Full normalized-package/consumer checks are a separate gate from
the package-inventory regression already included in the verifier.

Registry observations were read directly from the [nbreq API](https://crates.io/api/v1/crates/nbreq),
[winpoll API](https://crates.io/api/v1/crates/nbreq-winpoll) and Darwin API endpoint on 2026-09-10.
Cargo's [versioned path-dependency rules](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#multiple-locations)
explain why local support compilation does not establish registry readiness.

Further broad F5 optimization, richer DNS routing support, new HTTP convenience features and
GDS device tuning are not proposed additions to the 0.2 release scope here. Any new finding in
the release gates must still be fixed or explicitly resolved before publication.
