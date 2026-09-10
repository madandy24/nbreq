# NBReq 0.2.0 release checklist

Opened 2026-09-10. Status: R1 verified; R2 platform proof verified with explicit Darwin override;
R3 has a clean tested Darwin candidate; local root candidate preparation is in progress.
Registry-only acceptance remains open. A user-reported nbreq/Wine issue is untriaged: passing
tests are evidence for their exercised hosts/workloads, not authoritative release acceptance.
GDS is blocked on unrelated work. Its application/device profile is not a prerequisite for
releasing nbreq's general-purpose library. Consumer documentation/examples and external checks
are authorized; publication remains separate. This checklist joins the [follow-up programme](project_nbreq_followup_plan.html)
and [memory work](nbreq_memory_plan.md); it does not replace their historical evidence.

## Resume checkpoint

| Field | State |
| --- | --- |
| Candidate | Root manifest is 0.2.0, Rust 1.85 / edition 2024. Scoped M2.5/M3/R1 work is isolated on `codex/nbreq-0.2.0-release`, based on clean Darwin commit `4a6c807a49f72ad9ff6585b0f9b479b61fee6609`. This is a review checkpoint, not final release acceptance. Main's unrelated F5 work remains separately owned. |
| Verified core | F1/F2 Resolver and TCP, F3 private DNS codec, F4 feature boundary, F6 bounded macOS support, F7 HTTP convenience, review fixes and memory M0–M3 have accepted evidence. |
| Source recheck | Before R1, all 119 ordinary inputs in the final M3 E manifest matched on 2026-09-10. R1 now changes docs, example registration/examples and source doc comments/includes; it does not change runtime implementation. The new release rehearsal freezes its own manifest. |
| Registry recheck | crates.io API reports nbreq 0.1.1 and nbreq-winpoll 0.1.0; nbreq-darwin returns 404. Root 0.2.0 is not published. |
| Package inventory | Root and Darwin `cargo package --list --allow-dirty --offline` succeed. New body-budget source/tests and retained fuzz seeds are included; thoughts, GDS evidence and F5 tools are excluded. This does not prove normalized-package compilation or registry resolution. |
| Active slice | The clean Darwin archive passes stable/MSRV helper tests and stable Clippy on both Macs. Root Windows verifier initially exposed error 10054 in a UDP test fixture; a bounded socket reproduction confirmed closed-peer ICMP behavior. The Windows-only fixture correction passes six route-change tests and the full 24-step verifier (128.456 s). No runtime code changed in this candidate-preparation slice. |
| Evidence | [Artifact manifest](evidence/nbreq_020_artifacts.json) and [archive](evidence/nbreq-020-consumer-20260910.tar.gz), SHA256 e2425b7b46abed9a7a9d673da03b95d80e85efed64c5e2feaafed52d5f1a7f85. Frozen source/package inputs rechecked after all runs. |
| Next slice | Commit the scoped root checkpoint and identify its package; preserve failures as well as passes. Record and investigate the user-reported nbreq/Wine issue before treating test results as release acceptance. R4's final mixed-capability soak and R5's exact-candidate/hosted-CI gates remain. Darwin publication still requires separate authorization. |
| GDS boundary | Preserve its source and installed DLL. M4 device/application memory acceptance waits for GDS availability. Do not export GDS policy as nbreq defaults. |

## Remaining work

Status is deliberately split between existing evidence and final release acceptance.

| ID | Status | Work and exit condition |
| --- | --- | --- |
| R1 — Consumer contract and documentation | Verified on Windows, Linux and both Macs | Scoped API audit, migration notes, 0.2 snippets, ownership/budget/error guidance, DNS/TCP walkthroughs, platform/feature matrix and Darwin limitations are in place. Seven examples are registered and packaged. The final release candidate must remove the unreleased wording. |
| R2 — Independent consumer proof | Platform proof verified with explicit Darwin override; registry gate open | External temporary workspaces on all four hosts exercise ordinary default/native-only/minimal/test-support APIs against normalized archives. Stable and Rust 1.85 pass fresh online dependency resolution, HTTP/callback/manual/cancel/shutdown, streaming, TCP, retained budgets and feature probes. The same three HTTP tests also pass against registry 0.1.1. All seven packaged examples run on each host, including live DNS/HTTPS. The four independently resolved locks are identical. R3 publication is still needed for registry-only acceptance. |
| R3 — Support crates and package graph | Clean Darwin candidate tested; publication remains open | Darwin commit `4a6c807` is isolated and clean. Its 11,377-byte archive SHA256 is `656cf22c96ceb3f0ab3b96b24a3465021e22483390a32cc34d201419674401c4`; the exact archive passes five helper tests on both Macs/stable/MSRV and stable Clippy. Winpoll is unchanged from the released source. Earlier advisory/license gates remain applicable while the graph is unchanged. After separate authorization/publication, prove registry-only root resolution. |
| R4 — Final reliability observation | Open; earlier evidence reusable | Reconcile F5.1–F5.4 with M1–M3 instead of repeating completed memory experiments. Retain the planned final multi-hour mixed HTTP/Resolver/TCP soak on the supported platform set, including sustained pressure, retained bodies, cancellation and shutdown. Check for a missing 0.1.1 common-path comparison before making release performance claims. Record platform-specific interruptions/recovery where practical. |
| R5 — Exact release candidate | Local checkpoint preparation; acceptance open | Freeze the reviewed source/docs/locks while preserving unrelated dirty work. The corrected Windows candidate verifier passes. Final matrix/hosted CI, x86 companions as applicable, advisory/license gates, archive tests, consumer proof and docs.rs/package-link checks remain release gates. Reconcile the Wine finding with the relevant test oracles before claiming acceptance. |
| R6 — Root publication and registry smoke | Separate owner action after R1–R5 | After R3's authorized support publication and the registry-resolved root proof, publish nbreq 0.2.0. Inspect the published metadata/docs and run a clean consumer using only the registry release. Publication is not implied by this checklist. |

R3's support publication necessarily precedes the final registry-only root rehearsal in R2/R5.
Before that point, a clearly labelled local support override can validate code, but cannot close
the registry-consumer gate. Dependency ranges may select different versions in a fresh consumer
than in our Cargo.lock; the advertised MSRV must be checked against that distinction.

## Open finding W-01 — nbreq on Wine

On 2026-09-10 the user reported an nbreq/Wine bug and cautioned that existing tests may not be
authoritative until it has been examined. Symptoms, reproducer and affected test assumptions
are not yet available in this task. Continue reversible candidate preparation; do not equate
passing native-host gates with Wine coverage or a resolved correctness concern. Once the
findings are available, establish the failure and inspect whether it exposes a product bug,
an unsupported platform assumption, or a test-oracle gap. No disposition is assumed here.

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
