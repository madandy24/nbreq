# R4 — final reliability observation

Opened 2026-09-14. User authorized starting R4 and reconnected all three bridges.
No publication or GDS source/runtime changes are included.

## Resume checkpoint

| Item | State |
| --- | --- |
| Worktree | `target/worktrees/nbreq-r4`, branch `codex/r4-reliability`. Fixture/harness commit `09ed5a63b853b25b502b5082ec0d22cb851611e4` extends `149450d6543376cdb8255b55d7eb286e98207f42`; comparison/collector commit `c5cc901` follows. Fixture and soak tooling are mirrored to main; the adopted comparison changes stay isolated to preserve main's separately owned F5 work. |
| Lab | `target/release-r4-20260914` in the main workspace. Preserve failures, source snapshots and per-host logs. |
| Fixture | Two cleanup regressions failed on extracted original behavior: no connection hangs; incomplete request head panics before reporting setup failure. Bounded accept/read cleanup and explicit request-observation checks make both green. Original stalled-response timeout check remains green; runtime deadlines and its 40 ms/500 ms assertions are unchanged. |
| Companion | Manual Engine request expires before first drive; no socket opens and the unused fixture joins. Demonstrates the valid path behind the earlier hang. |
| Other historical failures | Full default library suite passed after the fixture change; 60 repetitions of the six historical cases passed with four concurrent bounded children and immediate panic output. The five original DNS/TCP failure causes are not established; do not attribute them conclusively to load or OS discovery. |
| Bridges | Linux `gds-client-01i linode`, Intel `intel-mac`, ARM `scaleway-nbreq` returned successful identity checks. Linux default Rust 1.85; Macs default stable 1.98. Windows/ARM four-hour jobs started at approximately 03:45 UTC September 14; Intel at 03:51. |
| Next | All four four-hour soaks and the complete Windows x86 companion passed. All five archives are local, with every archived file hash and every long-run cycle record independently checked. Windows common-path comparison and longer 1 KiB follow-up passed; Linux comparison build request `20260914-081957-2080a3cd` is queued. Check its terminal reply before further bridge work. No host soak remains active. |

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
| Linux | Running: `/home/ubuntu/nbreq-r4-e78b73b18d4c/observations/soak`, supervisor PID 1443783, request `20260914-041003-c02b96fe` completed/ok/exit 0 at 04:10 UTC. Full preflight passed in 1041.905 seconds on Rust 1.85, including all 900 fixture connections, corruption/watchdog checks and both rehearsals. Default binary SHA256 `7622e19154ece67d2dcd362e155b72a1fb29f38d77292d6e90a0413b1fb19b51`. |

Heartbeat `finish-nbreq-r4-reliability-checks` checks this task every 15 minutes, continues the
authorized gates, and stays quiet on unchanged healthy progress. Pause it when the observations
and remaining R4 work are complete or explicitly handed back for user disposition. Keep later
Windows x86 builds and the common-path comparison out of the active host's soak interval.

04:09 UTC inspection: Windows, Intel and ARM have completed approximately 24, 18 and 24 minutes
respectively, with exact expected terminal accounting and zero quiescent operation/body/queue
gauges. The nonzero failed-request counters are the deliberately provoked response-budget
refusals (one in the first round and every tenth round), not unexpected errors. Public DNS and
TCP failure counters are zero. No Windows supervisor sampling/clock anomaly has been logged.
These are interim observations, not acceptance. The bridge results are copied into the local
lab's `bridge-results` directory; Linux's final preflight and launch evidence are included.

04:45 UTC monitoring still finds all four jobs healthy. `polls-e78-7-checked.json` validates
interim terminal counts and quiescent gauges from retained bridge replies. Windows sampled
private bytes have risen from roughly 4.9 MB in rehearsal to 7.1 MB around one hour, while its
working set remains around 16 MB; other hosts' sampled RSS is approximately stable over these
checks. These different measures do not establish a leak or device fit. Inspect the complete
trend before acceptance, and isolate allocator/platform retention if growth does not settle.
An additional read-only Windows process snapshot at 04:46 UTC records 12 threads and 294 handles
for client PID 20784 in `windows-live-process.jsonl`; future snapshots can check owner counts.

## Completed-run collection checkpoint (07:55 UTC)

Windows and both Macs have passed the complete 14,400-second workload, including final Engine
shutdown, expected live-stream cancellation, fixture joins and four unforced child exits.
`tools/release-soak/collect.py` validates final results, binary hashes and rehearsal results before
packing raw logs while excluding build trees. This post-processing tool is new; it does not
change any frozen soak input. Bounded bridge chunk reads copy the archives locally, validating
both chunk and complete-archive hashes; request and transfer records remain in the lab.

| Host | Local raw archive | SHA256 |
| --- | --- | --- |
| Windows x64 | `windows-e78-evidence.tar.gz` (1,250,807 bytes) | `21d90c26c138c853ab85072ed1b480ffecca2e567a4dd7d938794396da1d88c8` |
| ARM Mac | `mac-arm64-e78-evidence.tar.gz` (1,289,832 bytes) | `4ff6d1f1890c3e994dc0c921dd7fb7d41fe11e69374a384aa9489578cb3d669a` |
| Intel Mac | `mac-x64-e78-evidence.tar.gz` (1,387,932 bytes) | `2018038b6172f26f88409efbd56820facbf33f5c48d7fab8468e150125bdd663` |

These live beneath `target/release-r4-20260914`. Final archive-content verification and full
trace analysis remain. Windows completed 17,462 rounds, 358,214 accepted main-Engine HTTP
operations before the separately checked final shutdown stream, and 34,924 TCP exchanges.
Its 1,747 failures and 36,671 cancellations are exactly the deliberate workload terminals.

The x86 job sets `CARGO_BUILD_TARGET=i686-pc-windows-msvc` for the entire verifier as well as
passing `--target` to the soak build. Logs show the i686 test binaries actually executing;
all 24 stages passed in 178.927 seconds. Its default and native-only release rehearsals and
900-connection/negative supervisor checks must finish before closing the x86 companion.

For resumption: `download_evidence.py` in the lab reads `*-pack-request.json`, records each bridge
request and supports reusing completed/pending chunk requests. `preserve_bridge.py` and
`inspect_progress.py` retain/validate interim replies; the latter does not accept a completed
soak. Linux collector upload is already complete at `/tmp/nbreq-r4-collect-20260914.py`; package
only after Linux's final result passes, then retrieve its archive using the same bridge flow.

## Completed soak acceptance (2026-09-14)

All four 14,400-second default-capability observations passed. Each of 73,593 complete cycles
was re-read from the archived client event stream and checked for contiguous numbering,
exact expected refusal/cancellation accounting, zero quiescent gauges, bounded high-water
marks and consistent connection ownership. Across the four main Engines, 1,509,627 HTTP
operations were accepted and 147,186 standalone TCP exchanges completed. Final live-stream
shutdown is checked separately and is not included in those main-Engine pre-shutdown counts.
All child exits were unforced with readers and fixtures joined. No supervisor sampling/clock
anomaly or child stderr was recorded. Public-network rounds all passed.

| Host | Cycles | Accepted HTTP | Completed HTTP | Expected cancellations / refusals | TCP exchanges | Public rounds |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| windows | 17,462 | 358,214 | 319,796 | 36,671 / 1,747 | 34,924 | 237 |
| mac-arm64 | 17,712 | 363,339 | 324,371 | 37,196 / 1,772 | 35,424 | 237 |
| mac-x64 | 18,602 | 381,584 | 340,658 | 39,065 / 1,861 | 37,204 | 237 |
| linux-x64 | 19,817 | 406,490 | 362,892 | 41,616 / 1,982 | 39,634 | 238 |

These numbers describe the workload on each host, not comparative platform throughput.
All hosts reported the same peak accounted buffered capacity (2,113,536 bytes), streaming
reservation (16,384 bytes), TCP reservation (16,384 bytes), eight HTTP connections and ten
waiting requests. Final operation/body/stream/TCP/queue/waiter gauges were zero; six idle
HTTP connections remained immediately before the explicitly checked Engine shutdown.

Process measurements below cover the client only, with separate fixture processes. MiB means
1,048,576 bytes. Samples are nominally two seconds apart (largest observed gap below 2.21 s).
RSS/working set is not live Rust heap. Windows private bytes measure private commitment;
Linux private bytes sum resident Private_Clean/Private_Dirty; macOS private is unavailable.
The two private-byte measures are not equivalent.

| Host | Sampled peak RSS MiB | OS lifetime peak RSS MiB | Sampled peak private MiB | Hour 1 / 2 / 3 / 4 mean RSS MiB |
| --- | ---: | ---: | ---: | --- |
| windows | 16.43 | 18.44 | 7.64 | 15.40 / 15.39 / 15.75 / 15.79 |
| mac-arm64 | 14.70 | unavailable | unavailable | 14.47 / 14.53 / 14.60 / 14.66 |
| mac-x64 | 9.06 | unavailable | unavailable | 8.84 / 8.64 / 8.74 / 8.74 |
| linux-x64 | 9.07 | 9.09 | 5.46 | 9.01 / 9.07 / 9.07 / 9.07 |

Windows mean private commitment by hour was 6.06 / 6.70 / 6.90 / 6.99 MiB. Its rate of growth
fell substantially; the last two hourly mean working sets differed by about 45 KiB. Linux RSS
was flat during the last two hours and private resident memory rose only about 7 KiB between
their means. ARM RSS increased gradually by roughly 184 KiB between the first and fourth
hourly means; Intel's last two means differed by about 7 KiB. This supports bounded resource
use during this workload. It does not prove zero slow leakage or acceptance on a 128/256 MB
device. The public network, allocator and OS remain part of these process observations.

The owner reports substantial other work on Windows during this run. Retain its correctness,
cleanup and process measurements, but treat Windows performance timings as observations from
a busy host. Prefer the separately obtained Linux comparison for timing interpretation; do
not invent an idle-host guarantee for either platform.

Windows x86 also passed the complete 24-stage verifier (178.927 s), 900 new fixture connections,
corruption/watchdog checks, 30-second default and 180-second native-only release rehearsals.
The entire verifier inherited `CARGO_BUILD_TARGET=i686-pc-windows-msvc`; logs show actual i686
test binaries. Its full preflight took 527.669 s. This is a short companion, not a four-hour x86
soak. Default/native binary hashes are retained in the archive and machine-readable analysis.

Every file in all five archives was hash-checked against its internal manifest before safe
extraction. `analyze_evidence.py` rechecks the cycle traces and computes hourly summaries.
Its first local attempt used the wrong preflight duration field name; the resulting KeyError
is retained as `analysis-initial-schema-error.log`. Correcting that post-processing field
did not change any test, binary, measurement or acceptance assertion.

| Archive under the R4 lab | Bytes | SHA256 |
| --- | ---: | --- |
| `windows-e78-evidence.tar.gz` | 1,250,807 | `21d90c26c138c853ab85072ed1b480ffecca2e567a4dd7d938794396da1d88c8` |
| `mac-arm64-e78-evidence.tar.gz` | 1,289,832 | `4ff6d1f1890c3e994dc0c921dd7fb7d41fe11e69374a384aa9489578cb3d669a` |
| `mac-x64-e78-evidence.tar.gz` | 1,387,932 | `2018038b6172f26f88409efbd56820facbf33f5c48d7fab8468e150125bdd663` |
| `linux-x64-e78-evidence.tar.gz` | 1,458,647 | `598eb45d7343dca5f24db5e968fcb47c7cb9148288d38a434efbe5aa696f048f` |
| `windows-x86-e78-evidence.tar.gz` | 84,282 | `e485299d159885011277907237f0679ae85ac0bed45393039d3966c364283a33` |

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

## Scoped registry comparison checkpoint

Commit `c5cc901` adopts the reviewed shared native-HTTP seam into the isolated R4 worktree
before timed collection. Main's separately owned F5 files are preserved. The current-side
tool lock was resolved offline under Rust 1.85; registry 0.1.1 keeps its independent lock and
checksum `dcce8e27aead7ca63e8d3d4f93d2cab59290e746cac2dfd92eec31b8517f9c90`.
Both sides use blocking accepted fixture sockets, TCP_NODELAY and bounded writes. No runtime
library code is changed. Missing legacy Resolver/TCP gauges remain unavailable.

The 131-file comparison source manifest is
`033d1fce1db88e8161f0ef4ab2ec5f30df613a97b39248fe8940d605bb17f9b1`;
its archive SHA256 is `16019ed9b7cf00c47bfc68eb8dbec1fb3c79d07c1e59845cedf75c84a3e098b4`.
All 120 original R4 inputs are byte-identical; the other 11 files are benchmark tooling.
Library identity remains the e78 manifest above. The registry commit is
`a0599c87500ce6bb7499546b914faa771dd49438` (the commit behind the annotated v0.1.1 tag).

Automatic approval review twice blocked the Linux source transfer pending specific approval.
The owner explicitly approved the exact archive and `/tmp/nbreq-r4-compare-033d1fce1db8.tar.gz`
destination; upload `20260914-081909-67131e99` then passed. No transfer was queued during the
blocked attempts. Linux build launched successfully as PID 1546690 through request
`20260914-081957-2080a3cd`, beneath `/home/ubuntu/nbreq-r4-compare-033d1fce1db8`.

The owner also explicitly authorized the dedicated Apple Silicon Mac as a comparison host
and the necessary uploads. This extends the original Windows/Linux timing scope, while
retaining within-host comparisons only. ARM upload `20260914-082236-f1e6c281` passed;
build request `20260914-082322-692c198a` targets `/Users/m1/nbreq-r4-compare-033d1fce1db8`.
Inspect its terminal reply before further bridge work. Neither remote build overlaps its soak.

Each host builds four Clippy-clean, locked release binaries: current/registry crossed with
plain/allocation instrumentation. There are 36 bounded launches, three alternating repetitions
per version/instrumentation/body size. Each has 32 warmups and three measured samples of 4,096
requests (1/64 KiB bodies) or 256 requests (1 MiB). Exact bodies, connection reuse, applicable
quiescence and joined exit are checked. Plain timings and instrumented allocation counts have
separate roles. The in-process fixture and nominal 10 ms process sampling are explicit;
comparison process memory is not client-only memory and this is not an idle-CPU workload.

Windows completed all 36 launches. Initial plain median-of-launch-medians was 303.66 versus
258.51 ms for current/0.1.1 at 1 KiB; wide overlapping launch ranges made this inconclusive.
Ten longer alternating launches of the same plain binaries (32,768 requests per sample,
three samples, five pairs) also passed. The paired timing differences ranged approximately
from -3.2% to +2.7%; the median-of-launch-medians was 2,073.46 versus 2,025.82 ms (+2.35%).
Retain both observations. The owner subsequently confirmed substantial concurrent Windows
work, so neither is a clean performance baseline. Linux and dedicated ARM results remain open.
