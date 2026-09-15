# Main/R5 housekeeping

2026-09-15. Owner authorized housekeeping after the README/examples pass: reconcile both trees,
commit completed work, integrate the release history into main and prepare final verification.

## Resume checkpoint

| Item | State |
| --- | --- |
| Starting points | Main `2c382b7`; R5 `b3abd5c`, 18 commits ahead and descended from main. Both have the same current library/test/support/root dependency inputs after line-ending normalization. |
| Safety checkpoint | Saved all 147 changed/untracked paths in **`cb18acc301520ec6d250a1eb467b98f2cc2a8c99`**, local branch `codex/main-before-housekeeping-20260915`. Inventoried 313 files and verified snapshot contents; ignored builds and other worktrees are preserved. |
| Reconciliation | Adopted main's memory benchmark knobs/gauges, F5 lock, ignore entry, later Wine/consumer/memory reports and missing evidence. Retained R5's comparison runner, fixture fixes and quiescence check. Release wording is consistent in packaged docs. Both adopted historical archives match their manifests. |
| Presentation | Committed as **`655a6e3`**: accepted README and 17 examples, their CI/package tooling and verification reports. All 81 library/test/support/root-manifest/lock inputs match main's saved state after line-ending normalization. |
| Validation | Benchmark reconciliation passes Windows stable formatting, warning-denied all-feature lint and builds for current, registry 0.1.1 and memory tools. Both observers pass small correctness/cleanup runs; HTTP and HTTPS memory runs exercise a 128 KiB budget and prove nonzero high-water reservation followed by zero retained charge. Full Windows verifier, examples and three clean normalized packages are next. No performance baseline or repeated long soak is needed. |
| Integration | Fast-forward main to the completed R5 history after the snapshot and reconciliation are verified. Leave main and R5 clean; retain the snapshot branch. |
| External actions | Existing R5 evidence-archive upload approval remains pending. No crate publication is implied. Any public push must cover the concrete newly included evidence payload. |

Main's F5 lock retains its existing compatible graph rather than adopting unrelated build-tool
updates from the separate R4 comparison run. Frozen R4 archives remain the authority for those
historical measurements. Later main reports and archives fill provenance gaps in R5; historical
records remain historical, with this checkpoint and the release checklist recording current state.

Tool checks are under main's ignored `target/housekeeping-20260915/tools-check-b`; the first
attempt is retained in `tools-check`. Its observer succeeded, but the temporary controller tried
to parse progress lines as JSON. The corrected controller selects the single JSON record; no
repository implementation changed because of that harness error. These tiny runs establish
correctness and cleanup only, not comparative performance.
