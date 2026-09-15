# Main/R5 housekeeping

2026-09-15. Owner authorized housekeeping after the README/examples pass: reconcile both trees,
commit completed work, integrate the release history into main and prepare final verification.

Subsequent release: **nbreq 0.2.0 is published and tagged at `d866179`**, with registry-only
platform/MSRV and x86 checks complete. See [publication](nbreq_020_publication.md) for current
state. This report retains the earlier housekeeping source and package identities.

## Resume checkpoint

| Item | State |
| --- | --- |
| Starting points | Main `2c382b7`; R5 `b3abd5c`, 18 commits ahead and descended from main. Both have the same current library/test/support/root dependency inputs after line-ending normalization. |
| Safety checkpoint | Saved all 147 changed/untracked paths in **`cb18acc301520ec6d250a1eb467b98f2cc2a8c99`**, local branch `codex/main-before-housekeeping-20260915`. Inventoried 313 files and verified snapshot contents; ignored builds and other worktrees are preserved. |
| Reconciliation | Adopted main's memory benchmark knobs/gauges, F5 lock, ignore entry, later Wine/consumer/memory reports and missing evidence. Retained R5's comparison runner, fixture fixes and quiescence check. Release wording is consistent in packaged docs. Both adopted historical archives match their manifests. |
| Presentation | Committed as **`655a6e3`**: accepted README and 17 examples, their CI/package tooling and verification reports. All 81 library/test/support/root-manifest/lock inputs match main's saved state after line-ending normalization. |
| Validation | Complete at **`432509b8d2d198cb0139def322f2cc82cd4a76c6`**: full Windows verifier 24/24; all 17 examples build and all 16 local execution cases pass; three clean normalized packages pass inventory/link checks; external packaged consumers pass all 22 stages across stable Rust 1.97.1 and Rust 1.85, including feature-presence/absence checks, registry 0.1.1 compatibility and 16 packaged-example cases. Benchmark formatting/lint/builds, two observer correctness runs and HTTP/HTTPS 128 KiB budget/gauge checks also pass. |
| Integration | Main fast-forwarded successfully from `2c382b7` to validated source **`432509b`**, with a clean working tree. The final report/evidence commit also advances main by fast-forward. The snapshot branch, other worktrees and ignored caches are retained. GitHub main `5441d20` is an ancestor, so the public update requires no history rewrite. |
| External actions | Owner explicitly approved pushing main and running CI, including all four evidence archives (9,889,225 bytes). Main **`451a7699248c60359dbda6b221e0dc9daa544719`** was pushed and confirmed by `git ls-remote`. [Hosted run 34943196228](https://github.com/madandy24/nbreq/actions/runs/34943196228) checks that exact commit. Crate publication remains separate. |
| Final hosted result | **10/10 jobs pass**, with the initial Intel Mac stable timeout and one unchanged failed-job rerun retained below. Eight full 24-step verifiers, 128 example execution cases, 136 consumer stages / 464 tests and 32 expected feature-absence probes pass. Both distinct fresh-consumer locks and the advisory DB/tool/root-lock identity match the previously audited R5 evidence. [Verified results](evidence/nbreq_housekeeping_hosted.json). |

Main's F5 lock retains its existing compatible graph rather than adopting unrelated build-tool
updates from the separate R4 comparison run. Frozen R4 archives remain the authority for those
historical measurements. Later main reports and archives fill provenance gaps in R5; historical
records remain historical, with this checkpoint and the release checklist recording current state.

Tool checks are under main's ignored `target/housekeeping-20260915/tools-check-b`; the first
attempt is retained in `tools-check`. Its observer succeeded, but the temporary controller tried
to parse progress lines as JSON. The corrected controller selects the single JSON record; no
repository implementation changed because of that harness error. These tiny runs establish
correctness and cleanup only, not comparative performance.

## Final local evidence and release boundary

The [136-file archive](evidence/nbreq-housekeeping-20260915.tar.gz) and
[manifest](evidence/nbreq_housekeeping_artifacts.json) preserve snapshot inventories,
reconciliation hashes, exact controller/tool inputs, checks and logs, consumer locks and the
three frozen `.crate` files. Archive SHA256:
`3188665aba998abac82a9b3fec8c7455b3ca76dff90c3445ab007329eb655d49` (523,926 bytes).
The validated source/package commit is `432509b`; the subsequent reporting commit changes
only excluded `thoughts/` records, so it must not be presented as the archive's source identity.

| Candidate | SHA256 |
| --- | --- |
| nbreq-darwin 0.1.0 | `31d7a69844b990331f85dc497f14d0b6a08f8ceaee402e7c6aa19919f3e3c839` |
| nbreq-winpoll 0.1.1 | `9e4fc64245514209beb6e3feccb4b980e1c259546a9e07543e3529e1d7053696` |
| nbreq 0.2.0 | `59b54d263c6951da867df745d5fc1cd9aad29930c659a9f9d378c7bfc6966860` |

Root package verification uses explicit local helper overrides; external consumers use the
exact unpacked helper archives. This is **not registry-only evidence**. Local package checks
used Windows x64; the subsequent hosted run covers Windows/Linux x64 and both Mac architectures
on stable Rust 1.98.1 and Rust 1.85. This pass makes no timing claims or new Wine claim.

The earlier automatic-review rejection concerned a different approved payload. The owner then
explicitly answered **"Approve push and CI"** for main `451a769`, including the R5 technical,
Wine DNS, older clean-candidate and new housekeeping archives, **9,889,225 bytes** total.
The normal fast-forward push to `https://github.com/madandy24/nbreq` main succeeded; the exact
remote commit was independently confirmed. Archive hashes and inventories are recorded;
the inherited archive audit found no credential-shaped matches. The local snapshot branch
was excluded. Helper/root publication and the release tag occurred later under the owner's
continuation instruction; their exact identities and verification belong to the publication record.

## CI observation — Intel Mac fragmented-response deadline

First hosted attempt: nine jobs passed. Intel Mac stable (macOS 15.7.9, Rust 1.98.1) passed
416 ordinary-default library tests, then the existing
`fragmented_and_chunked_responses_complete_through_the_public_api` test hit its two-second total
request deadline at `tests/http_adversarial.rs:780`. The other 15 adversarial tests passed.
The log does not identify whether the fixture had finished writing, whether the client was
waiting for progress, or whether scheduling consumed the deadline. **Cause remains unconfirmed.**

The first run/job logs and all eight available artifacts are retained under
`target/housekeeping-20260915/hosted`; only the failed job was rerun, with diagnostic logging,
on unchanged `451a769`. No assertion, timeout, test selection or library code was changed.
A passing retry does not establish host load as the cause or rule out a timing-sensitive defect.
If this repeats, capture monotonic fixture accept/request/write milestones and client progress
before considering a timeout change. The current logs do not justify a speculative fix.

The unchanged rerun passed the full verifier, all 16 example cases and both fresh-consumer
cases. [The final result record](evidence/nbreq_housekeeping_hosted.json) includes the original
failure excerpt, final job/step records, source/lock checks and hashes of both attempts' raw
logs/artifacts. Raw hosted downloads remain local in the directory above; the first attempt
was preserved before the rerun. A temporary result checker initially assumed every example
should exit zero; the deliberate response-limit failure correctly exits one. The checker now
compares all 16 expected exit codes with the established local cases; no example changed.

The final follow-up commit contains only excluded `thoughts/` records and CI evidence metadata.
It uses `[skip ci]` to avoid repeating the unchanged matrix; `451a769` remains the exact hosted
source and `432509b` the exact frozen package source. The public push approval gap is resolved.
