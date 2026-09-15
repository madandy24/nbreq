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
| Validation | Complete at **`432509b8d2d198cb0139def322f2cc82cd4a76c6`**: full Windows verifier 24/24; all 17 examples build and all 16 local execution cases pass; three clean normalized packages pass inventory/link checks; external packaged consumers pass all 22 stages across stable Rust 1.97.1 and Rust 1.85, including feature-presence/absence checks, registry 0.1.1 compatibility and 16 packaged-example cases. Benchmark formatting/lint/builds, two observer correctness runs and HTTP/HTTPS 128 KiB budget/gauge checks also pass. |
| Integration | Main fast-forwarded successfully from `2c382b7` to validated source **`432509b`**, with a clean working tree. The final report/evidence commit also advances main by fast-forward. The snapshot branch, other worktrees and ignored caches are retained. GitHub main `5441d20` is an ancestor, so the public update requires no history rewrite. |
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
exact unpacked helper archives. This is **not registry-only evidence**. This pass used Windows
x64 only and made no timing claims. The earlier R5 hosted matrix and actual Wine checks remain
historical evidence; the new examples/CI step still need the updated hosted run.

Public push remains pending because automatic approval review previously rejected the new
R5 evidence-archive upload: earlier authorization covered a different payload. Housekeeping
authorizes this local reconciliation, not an implicit expansion of that public upload. The
concrete push to `https://github.com/madandy24/nbreq` main would include the R5 technical,
Wine DNS, older clean-candidate and new housekeeping archives, **9,889,225 bytes** total,
along with the reviewed source/docs/history. Archive hashes and inventories are recorded;
the inherited archive audit found no credential-shaped matches. The local snapshot branch
is excluded. After approval, push main and check the updated hosted matrix; helper/root
publication and release tags still require their own authorization.
