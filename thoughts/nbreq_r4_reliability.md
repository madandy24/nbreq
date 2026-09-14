# R4 — final reliability observation

Opened 2026-09-14. User authorized starting R4 and reconnected all three bridges.
No publication or GDS source/runtime changes are included.

## Resume checkpoint

| Item | State |
| --- | --- |
| Worktree | `target/worktrees/nbreq-r4`, branch `codex/r4-reliability`, commit `09ed5a63b853b25b502b5082ec0d22cb851611e4` over `149450d6543376cdb8255b55d7eb286e98207f42`. Scoped fixture/tool changes are mirrored to main; main's unrelated work is preserved. |
| Lab | `target/release-r4-20260914` in the main workspace. Preserve failures, source snapshots and per-host logs. |
| Fixture | Two cleanup regressions failed on extracted original behavior: no connection hangs; incomplete request head panics before reporting setup failure. Bounded accept/read cleanup and explicit request-observation checks make both green. Original stalled-response timeout check remains green; runtime deadlines and its 40 ms/500 ms assertions are unchanged. |
| Companion | Manual Engine request expires before first drive; no socket opens and the unused fixture joins. Demonstrates the valid path behind the earlier hang. |
| Other historical failures | Full default library suite passed after the fixture change; 60 repetitions of the six historical cases passed with four concurrent bounded children and immediate panic output. The five original DNS/TCP failure causes are not established; do not attribute them conclusively to load or OS discovery. |
| Bridges | Linux `gds-client-01i linode`, Intel `intel-mac`, ARM `scaleway-nbreq` returned successful identity checks. Linux default Rust 1.85; Macs default stable 1.98. Windows/ARM four-hour jobs started at approximately 03:45 UTC September 14; Intel at 03:51. |
| Next | Windows and both Macs passed all frozen `e78b73b18d4c` preflight gates and are soaking. Linux's full verifier passed and its native release build is underway at 03:51 UTC. Each host runs the complete verifier, release/default/native builds with warning-denied Clippy, 900 fixture connections and two negative supervisor checks, then 30-second default and 180-second native rehearsals. Start Linux's four-hour default soak only after its complete preflight passes. |

## Frozen intent for this slice

- Four-hour mixed workload on ordinary Windows, Linux, physical Intel Mac and physical Apple
  Silicon Mac. These are reliability observations, not cross-host performance rankings.
- Use ordinary public constructors and the default capability set on one long-lived Engine;
  native-only gets a separate shorter companion. No private backend or injected production hook.
- Repeated concurrent small buffered HTTP/HTTPS, fixed upload, bounded streaming/slow consumption,
  body-capacity retention and deliberate budget refusal followed by healthy small work, standalone
  TCP byte/half-close checks, public DNS, cancellation and periodic joined Engine churn.
- Client and HTTP/TLS fixtures run in separate processes. Fixture ownership must remain bounded
  over connection churn; the existing short memory fixture's cumulative 256-connection limit and
  retained socket/JoinHandle collection are not suitable unchanged for a long soak.
- Validate exact successful bodies, expected terminal categories, bounds, quiescent operation/
  queue/body/TCP gauges and expected idle connection state. Joined shutdown and fixture/process
  cleanup are mandatory; a crash, missing completion or forced termination is a failed run.
- Timestamp progress and retain raw process-memory observations with their sampling limitations.
  Explicitly record suspension/elapsed-clock anomalies rather than silently calling wall time
  active test time. Do not deliberately interrupt GDS or host network settings.
- Public-network DNS/trust checks are labelled separately from controlled loopback cases. Any
  unexpected error remains evidence requiring disposition; never omit it from a successful report.

## Evidence reuse and remaining boundaries

M1–M3 already cover paired allocation/retention observations, large-transfer copies, TLS staging,
pressure reservations and correctness across the supported hosts. R1 covers release-facing docs
and compiled examples. R4 will not repeat that entire programme. Reconcile missing common-path
0.1.1 checks separately from the new mixed-capability soak; Wine pilot results are application
evidence, not benchmark data. Wider GDS admission/device profile tuning stays separate.

Final exact-candidate hosted CI and registry-only consumer acceptance remain R5/R2; support and
root publication remain R3/R6 with separate authorization. Update the [release checklist](nbreq_020_release_plan.md)
as this slice advances.

## Harness rehearsal and frozen preflight

The unpublished `tools/release-soak` tool uses one ordinary Engine and separate HTTP, verified
TLS and TCP echo processes. Its long-lived Engine has eight HTTP connections, a 4 MiB buffered
body cap, bounded streaming/TCP queues, 16-request small bursts competing with two held polls,
retained response aliases, deliberate admission/response budget refusals and joined lifecycle
checks. Public DNS and hostname HTTPS are recorded separately once per minute. No production
implementation or dependency changes are part of R4 so far.

Windows development evidence is retained under `target/release-r4-20260914`:

- `smoke-a` and `smoke-b` failed because the initial harness expected EngineStopped when shutting
  down accepted work. The documented terminal is Cancelled. Both failures and their tool-source
  snapshots are retained; the assertions were corrected without changing library behavior.
- `smoke-c` passed 12 cycles / 253 HTTP operations / 24 TCP exchanges, including public DNS/HTTPS,
  exact bytes, expected refusal/cancellation, quiescent gauges and all four processes joining.
  It is a ten-second **debug/unfrozen** rehearsal, not a release or performance observation.
- `harness-checks-c` passed 900 fresh fixture connections and detected deliberately corrupted
  bytes, then rejected cleanup because pipe EOF narrowly preceded Windows process-exit reporting.
  The supervisor now allows one second for ordinary exit before marking termination as forced.
- `harness-checks-d` passes all five checks: HTTP/TLS/TCP each survive 300 fresh connections with
  zero retained socket owners, corruption produces the intended failed run without forced cleanup,
  and the watchdog produces a failed run with only its owned client forcibly terminated. All
  child processes exit and reader threads join. A forced termination never passes a real soak.

Frozen source manifest SHA256:
`e78b73b18d4c3b969f7e022a705b8c84146dab44d39a2d14cbe1893242936914`.
The 120-file archive `nbreq-r4-e78b73b18d4c.tar.gz` is 419,633 bytes, SHA256
`be75f9420f727e7059291a419a6ab2870281ffcbffb4b773c8105b2de8ca25cb`.
It contains the scoped fixture fix and unpublished tools over `149450d`. Every host checks all
file hashes before work; each binary embeds the source identity, target, compiler and assertion
profile. Preflight and soak reports also hash the binary. Build commands and failures have their
own logs. A detached job does not keep the bridge occupied; its exact PID, stage and final result
remain inspectable. Build commands have a 20-minute bound; the soak has progress and whole-run
bounds and owns cleanup of its exact children.

Commit `09ed5a6` records this slice. Its only content difference from the frozen tool inputs,
apart from Git line-ending normalization, is removal of surplus blank lines at the end of
`process_sample.py`; runtime, fixture and executed Python statements are unchanged. The actual
run identity remains the full frozen manifest above, not an invented exact-commit binary claim.

| Host | Isolated preflight | Initial supervisor PID / launch request |
| --- | --- | --- |
| Windows x64 | `target/release-r4-20260914/windows-preflight-e78` | 34048 |
| Linux x64 | `/home/ubuntu/nbreq-r4-e78b73b18d4c/observations/preflight` | 1419765 / `20260914-033827-2eb11f35` |
| Intel Mac | `/Users/andrew/nbreq-r4-e78b73b18d4c/observations/preflight` | 96381 / `20260914-033828-a7dd5a7e` |
| Apple Silicon Mac | `/Users/m1/nbreq-r4-e78b73b18d4c/observations/preflight` | 57803 / `20260914-033828-03d1fbd3` |

All bridge uploads and launch requests completed successfully. These PIDs are provenance, not
permission to kill a later process that reuses the number.

At 03:47 UTC the Windows preflight is fully passed (429.051 seconds), including all 24 verifier
stages in 130.287 seconds. ARM preflight fully passed (348.605 seconds), including the 24-stage
verifier in 81.817 seconds. Intel's verifier passed 24/24 in 190.988 seconds and its short
rehearsals remain in progress. Linux's verifier subsequently passed 24/24 in 474.065 seconds
on Rust 1.85 and its release builds are running. All successful tests are
unfiltered; do not count an in-progress host as passed.

| Four-hour job | State / exact launch |
| --- | --- |
| Windows | Running: `target/release-r4-20260914/windows-soak-e78`, supervisor PID 34384. Passed preflight default binary SHA256 `d92747078e4ecaae717ef3cbf3b26f84ab63287b70363d8cb61d59a8209c6731`. |
| ARM Mac | Running: `/Users/m1/nbreq-r4-e78b73b18d4c/observations/soak`, supervisor PID 60249, request `20260914-034544-845bca3b` completed/ok/exit 0. Passed preflight default binary SHA256 `84a3c18bde2ec1809f62276c498c91eb05c5055f12200bfe03ba0f86ae4b5c3e`. |
| Intel Mac | Running: `/Users/andrew/nbreq-r4-e78b73b18d4c/observations/soak`, supervisor PID 98998, request `20260914-035108-08aa66c6` completed/ok/exit 0 at 03:51 UTC. Full preflight passed in 590.733 seconds. Default binary SHA256 `55707f1466a62a81cd4f05e6c8e7326a75620150b16437da92008ff406a03613`. |
| Linux | Not started; await complete preflight. |

Heartbeat `finish-nbreq-r4-reliability-checks` checks this task every 15 minutes, continues the
authorized gates, and stays quiet on unchanged healthy progress. Pause it when the observations
and remaining R4 work are complete or explicitly handed back for user disposition. Keep later
Windows x86 builds and the common-path comparison out of the active host's soak interval.

## F5 reconciliation

M1 supplies separate-process allocation/process baselines on all four hosts; M2–M3 supply measured
retention/copy improvements, pressure and mixed workloads, with repeated same-host before/after
checks on Windows/Linux. These cover the substance of F5.1's current-tree description, F5.2's
streaming/concurrent/mixed cases and F5.3's measured changes. R1 supplies consumer documentation,
examples and package work from F5.4. R4 retains its final mixed-capability multi-hour observation.

The exact-registry 0.1.1 common-native-HTTP seam in main's separately owned F5 tool has only a
758-response Windows rehearsal recorded; it is not a clean comparison baseline. M3 compares
against M2.4 B, not 0.1.1. Therefore **the Windows/Linux common-path 0.1.1 comparison remains
open** and must not be claimed from M1–M3 results. Keep it scoped to the shared native HTTP API,
same host/toolchain, repeat timings and plain versus allocation attribution separately; no
Mac/Wine ranking or new optimization programme is implied.
