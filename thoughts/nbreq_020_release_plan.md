# NBReq 0.2.0 release checklist

Opened 2026-09-10; updated 2026-09-15. Status: R5 technical/presentation checks, main integration/push, clean package freeze and updated CI complete; both support crates published; registry acceptance and root publication are in progress; R1 verified; R2 platform proof verified with local
support overrides; R3 has tested Darwin and winpoll candidates. The clean root checkpoint is
committed and packaged. R4 reliability is complete; exact-candidate CI and registry-only gates remain.
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
| Candidate | Root 0.2.0, Rust 1.85 / edition 2024. Main now includes validated checkpoint **`432509b`**, with the 0.2 README, 17 grouped examples and reconciled tooling/reports. Runtime graph remains R5 **`bdf5db5`**, including compatible rustls minimum 0.23.45. [Housekeeping](nbreq_housekeeping.md) records main's preserved snapshot, final local checks and package identities; [R5 report](nbreq_r5_readiness.md) retains the earlier hosted technical matrix. |
| Verified core | F1/F2 Resolver and TCP, F3 private DNS codec, F4 feature boundary, F6 bounded macOS support, F7 HTTP convenience, review fixes and memory M0–M3 have accepted evidence. |
| Source recheck | Before R1, all 119 ordinary inputs in the final M3 E manifest matched on 2026-09-10. R1 now changes docs, example registration/examples and source doc comments/includes; it does not change runtime implementation. The new release rehearsal freezes its own manifest. |
| Registry recheck | Darwin 0.1.0 and winpoll 0.1.1 were published on 2026-09-15; API/index records and downloaded checksums match the frozen candidates. Root 0.2.0 is not yet published. [Publication checkpoint](nbreq_020_publication.md) owns current registry proof and receipts. |
| Package inventory | Three fresh clean **`432509b`** packages include patched dependencies and the final README/examples; all inventories/links and Windows stable/MSRV packaged consumers pass. Exact hashes and archives are in the [housekeeping manifest](evidence/nbreq_housekeeping_artifacts.json). Earlier pre-R5 root archives permit vulnerable rustls versions and remain historical evidence only. Registry-only resolution remains open. |
| Active slice | Main housekeeping and updated hosted run complete at **`451a769`**: 10 jobs pass, including eight full verifiers, 128 example cases and 464 fresh-consumer tests. Initial Intel Mac stable fragmented-response timeout passed on one unchanged failed-job rerun; cause remains unconfirmed and evidence is retained in [housekeeping](nbreq_housekeeping.md). Earlier R5 Windows i686 proof and the unconfirmed initial x86 HTTPS assertion remain in the [technical report](nbreq_r5_readiness.md). |
| Evidence | The [R5 report](nbreq_r5_readiness.md), [manifest](evidence/nbreq_r5_artifacts.json) and [299-file archive](evidence/nbreq-r5-technical-20260915.tar.gz) retain exact source, both hosted runs, consumer locks, advisory identity, local failures/passes and mirror provenance. [Pre-R5](evidence/nbreq_pre_r5_artifacts.json), [R4](evidence/nbreq_r4_artifacts.json) and earlier evidence remain intact. |
| Next slice | Continue the owner-requested [publication pass](nbreq_020_publication.md): finish candidate consumer checks with registry helpers, then publish root and verify fully registry-only consumers and hosted docs. Both helper releases are already indexed with exact verified hashes. Main's original state remains preserved in **`cb18acc`**; the Intel Mac observation remains documented. |
| GDS boundary | Preserve its source and installed DLL. M4 device/application memory acceptance waits for GDS availability. Do not export GDS policy as nbreq defaults. |

## Pre-R5 pass — independent-review follow-up (2026-09-15)

The owner accepted the scoped follow-up and authorized implementation after updating this plan.
Use one clean branch based on R4 `68649ee`, preserving its Wine fix and all completed evidence.
Keep main's mixed working tree intact; mirror scoped fixes only after checking their original
contents. A reviewed remote branch checkpoint is part of this pass. No crate publication, tag,
main merge, GDS change or repetition of the completed four-hour soaks is implied.

| Item | State | Work and acceptance |
| --- | --- | --- |
| P5-01 — Planned-body reservation | Verified: 3 reds, 6 greens | Demonstrate growth beyond an unused reservation, including a false budget refusal, retained excess charge, and failed growth preserving the original bytes/reservation. Reuse/extend the reservation exactly once while continuing to charge both real allocations during replacement. Prove final refunds and retain red/green logs. Current fixed-length HTTP prevents the problematic sequence; fix the helper rather than relying on that caller invariant. |
| P5-02 — Dependency policy | Verified: stable/MSRV consumers | Use Cargo-compatible version ranges for ordinary root/support runtime dependencies, retaining tested lower bounds and explicit default-feature choices. Keep dev/tool pins and the documented Rust-1.85 `time` exception. Preserve the reviewed root lock graph, then separately prove fresh default/native-only/minimal consumer resolution on stable and Rust 1.85, including a consumer constraint above a formerly exact dependency pin. Record resulting locks/versions; local helper overrides do not become registry-only evidence. |
| P5-03 — Compatibility documentation | Complete | Document rejection of ambiguous HTTP Transfer-Encoding plus Content-Length and errors for malformed required Windows adapter data; preserve both behaviors. Valid unaligned Wine records remain supported. Existing callback-activation ownership and duplicate-delivery/shutdown tests address the review concern; no speculative arithmetic change. |
| P5-04 — Packaging and published docs | Verified locally; publication checks assigned to R5/R6 | Use a fresh dedicated packaging directory and an explicit package path/hash inventory. Do not clean the broad target tree or use wildcard crate upload selection. Keep the stale `target/package` archive out of the release inputs. Verify candidate wording and link destinations; crates.io already rewrites relative README links, so no blanket conversion to moving `main` URLs. Carry exact post-publication README/docs.rs checks into R6. |
| P5-05 — Validation and durable checkpoint | Complete: verified and pushed | Run the relevant budget tests and complete Windows verifier; fresh consumer/MSRV checks cover the range changes. Retain source identities, locks and logs; commit and push the reviewed release branch without merging main or publishing crates. R5 still owns the final hosted cross-platform matrix and final-candidate package gates. |

The owner explicitly approved the public payload/destination on September 15 after the earlier
automatic-review rejection. Checkpoint **`72085093c88f12a5afcc9c2548a4b74dc305277b`** was pushed
to `origin/codex/pre-r5-release` at `https://github.com/madandy24/nbreq`; `git ls-remote` confirmed
the exact remote commit. The [checkpoint report](nbreq_pre_r5.md) retains the validation scope.

The two helper versions remain nbreq-darwin **0.1.0** and nbreq-winpoll **0.1.1**. Either helper
can publish first after separate authorization; both must actually resolve from the registry
before root registry acceptance. A fixed sleep is not evidence of index availability.
R5 must record the advisory database commit/date alongside audit JSON, including when
`cargo audit --no-fetch` omits that identity. R6 must inspect the published README and docs.rs
pages for stale unreleased wording and broken release-documentation links.

## R5 technical pass — verified 2026-09-15

The owner authorized R5 before the separate README/examples pass. The [R5 report](nbreq_r5_readiness.md)
records the passing hosted platform/stable/MSRV matrix, recorded fresh consumer graphs, Windows
x86 companion, advisory database identity and license comparison. The newly reported rustls
advisory was fixed by raising the minimum to 0.23.45. Final documentation, publication archive
identities and registry-only acceptance remain open.

The initial automatic-review rejection concerned a different approved archive payload. During
[housekeeping](nbreq_housekeeping.md), the owner explicitly approved all four included archives
and main push/CI. Main `451a769` is now public, including this technical report/archive and the
finished README/examples. The new hosted run checks the integrated presentation and tooling.

## Main-tree README and examples refresh — owner requested

The owner approved the A/B/C examples redesign and implementation, including an explicit verified
cancellation example and removal of the old programs. The [examples checkpoint](nbreq_examples_refresh.md)
records the 17 new programs, docs/package/tooling updates and Windows stable/MSRV plus live evidence.
Presentation commit `655a6e3` and housekeeping checkpoint `432509b` are integrated into main.
The [README presentation](nbreq_readme_refresh.md) is also implemented: convenience GET first,
short cancellation/DNS/TCP snippets, guide/example links, and curl history brief and last.
All four snippets compile on stable/MSRV and relative package links/anchors resolve.
Main `451a769` is pushed and all three clean `432509b` packages pass local consumer verification.

The completed presentation/package pass:

- Owner accepted the 0.2 README, restored Highlights and 17-program HTTP/DNS/TCP sequence.
- Examples, doctests, package allowlist, release wording and links pass; three fresh package
  identities are recorded in the housekeeping manifest. Earlier archives remain historical evidence.
- Integrated documentation is on the default branch. Actual crates.io README and docs.rs checks
  remain part of R6 after publication.

This refresh supplies new R5 evidence ahead of R6, beyond the original R1/pre-R5 checks.

## Release gates

Status is deliberately split between existing evidence and final release acceptance.

| ID | Status | Work and exit condition |
| --- | --- | --- |
| R1 — Consumer contract and documentation | Verified; final presentation integrated | Scoped API audit, migration notes, 0.2 snippets, ownership/budget/error guidance, DNS/TCP walkthroughs, platform/feature matrix and Darwin limitations are in place. The final owner-approved 17-example sequence supersedes R1's original seven examples. Main and packaged docs describe release 0.2.0. |
| R2 — Independent consumer proof | Platform proof verified with explicit Darwin override; registry gate open | External temporary workspaces on all four hosts exercise ordinary default/native-only/minimal/test-support APIs against normalized archives. Stable and Rust 1.85 pass fresh online dependency resolution, HTTP/callback/manual/cancel/shutdown, streaming, TCP, retained budgets and feature probes. The same three HTTP tests also pass against registry 0.1.1. All seven packaged examples run on each host, including live DNS/HTTPS. The four independently resolved locks are identical. R3 publication is still needed for registry-only acceptance. |
| R3 — Support crates and package graph | Darwin and winpoll candidates tested; publication remains open | Darwin `4a6c807` and Wine-fix winpoll `149450d` retain earlier platform proof. Pre-R5 `a44cac5` supplies fresh 8-file Darwin and 10-file winpoll candidates after compatible dependency-range changes, with Windows package verification. Winpoll's earlier x86/x64/MSRV and Wine proof remains historical until final-candidate checks. Root removes ipconfig/widestring; advisory and generated-license checks were refreshed. Publish both helpers only after separate authorization, then prove registry-only root resolution. |
| R4 — Final reliability observation | Complete within the documented scope | Four full native verifiers and four-hour default mixed observations, short default/native-only rehearsals, harness negative checks and complete Windows x86 companion pass. All 73,593 cycle records and archive hashes were independently checked. Common-native-HTTP registry 0.1.1 comparisons pass on Windows/Linux and the owner-requested dedicated ARM Mac (108 launches plus ten longer Windows small-body launches). Windows timings are qualified by the owner's concurrent work; no cross-host ranking. [Results and limitations](nbreq_r4_reliability.md) and [artifact manifest](evidence/nbreq_r4_artifacts.json) retain the proof. Fixture cleanup is fixed with reds/greens; five historical DNS/TCP causes remain unconfirmed. Runtime implementation is unchanged from the Wine fix. |
| R5 — Exact release candidate | Technical, presentation and package checks complete; registry gate open | Final main `451a769` passes the updated hosted matrix after one unchanged Intel Mac failed-job rerun, with original timeout/cause uncertainty retained. Three clean `432509b` packages include the accepted README/examples and pass links/inventory plus Windows stable/MSRV external consumers. Earlier Windows x86 and advisory/license proof remains applicable; both fresh locks match the audited R5 graphs. Registry-only proof waits for support publication. |
| R6 — Root publication and registry smoke | Separate owner action after R1–R5 | After R3's authorized support publication and the registry-resolved root proof, publish nbreq 0.2.0. Inspect the published metadata/docs and run a clean consumer using only the registry release. Publication is not implied by this checklist. |

R3's support publication necessarily precedes the final registry-only root rehearsal in R2/R5.
Before that point, a clearly labelled local support override can validate code, but cannot close
the registry-consumer gate. Dependency ranges may select different versions in a fresh consumer
than in our Cargo.lock; the advertised MSRV must be checked against that distinction.

R4 is complete; its [report](nbreq_r4_reliability.md) records fixture proof, qualified historical
failures, the four-hour/four-host contract and results, and the completed common-path comparison.

## 2026-09-14 release-path review

The clean fix worktree remains at `149450d6543376cdb8255b55d7eb286e98207f42`. A read-only
comparison finds no differences in its source, tests or support-crate files versus main
(normalizing line endings). Main still contains separately owned uncommitted tooling and
documentation; use the isolated candidate when freezing release inputs.

Recommended order:

1. R4 completed on September 14: F5 reconciled with M1–M3/R1, bounded fixture with red/green
   proof, qualified historical failures, four-host mixed soaks, x86 companion and shared-path
   comparisons. Preserve its exact evidence while assembling the final integrated candidate.
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

The [housekeeping checkpoint](nbreq_housekeeping.md) and [R5 technical report](nbreq_r5_readiness.md)
record current release validation. Earlier [R1/R2 consumer evidence](nbreq_020_consumer_readiness.md)
includes seven full 24-step verifiers and independent packaged consumers on all four hosts. [M3 E](nbreq_m3_memory_controls.md)
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
