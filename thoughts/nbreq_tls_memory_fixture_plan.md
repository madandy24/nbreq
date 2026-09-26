# TLS memory fixture follow-up

Opened 2026-09-26 NZST. This closes the optional ARM memory-observation gap without changing NBReq's production TCP/TLS implementation.

## Resume checkpoint

| Field | State |
| --- | --- |
| Authority | Owner approved replacing the Python TLS test server with Rust, then repeating the missing ARM observations. Existing Sol-worker/Astra-reviewer cycle continues. |
| Working branch | `codex/nbreq-smtp`, worktree `C:/User/projects/nbreq/target/worktrees/nbreq-smtp`, baseline `3f841be`. This branch includes the accepted TLS and SMTP work. Main and the earlier TLS branch remain unchanged. |
| Scope | Separate unpublished Rust loopback TLS server, Python process sampler/orchestrator, focused lifecycle/protocol tests, native measurements and durable evidence. No production library changes, mail submission or host runtime installation. |
| Active stage | MF3: candidate `6462562` passed the frozen Windows run; ARM/Linux stopped in controller fault tests before measurement. Astra approved the small portability correction after 11 passing controller tests and a focused pre-fix failure. Root freezes and reruns the corrected candidate on all three hosts. |
| Evidence lab | `C:/User/projects/nbreq/target/tls-memory-fixture-20260926/`. Preserve failed development checks as such, source and binary hashes, exact commands, raw process samples and actual negotiated protocols. |
| Baseline client | Existing `bede85c` release probe on ARM/Linux, unchanged source and executable where present. ARM SHA256 `76e9f6483bea26156dd85b91708ea3e221c6037e2e8a34d1acaf98cc17e187e5`; Linux SHA256 `f43abdad13da6594a7019f36c553d6c68c21affd7169b086e9b28e435fb33c98`. |
| Open limitation | Prior ARM Python3.9/LibreSSL2.8.3 server fails with `NO_SHARED_CIPHER`, including against an independent LibreSSL client. Local TCP connected and exchanged TLS bytes, so this evidence does not support port filtering. Exact internal LibreSSL cause remains unknown. |

## Stages

| ID | Work | Acceptance | Status |
| --- | --- | --- | --- |
| MF0 | Fixture and measurement contract | Explicit versions/counts, loopback and deadlines, subprocess ownership, unchanged sampled client | Accepted |
| MF1 | Rust fixture and observer replacement | Real verified TLS1.2/1.3, no Python SSL/OpenSSL dependency, bounded deterministic lifecycle | Implemented; seven independent Rust process tests passed |
| MF2 | Independent checks and review | Worker fixes findings; wrong-root/protocol/failure cleanup coverage; strict lint and Rust1.85 | Complete; Astra approved final source and inspected all independent local logs |
| MF3 | Frozen native observations | ARM 16/32 connections for explicit TLS1.2 and1.3; matched Linux and Windows checks as useful; source/client identities retained | In progress: frozen snapshot and native gates next |
| MF4 | Evidence and handoff | Reviewer accepts raw observations, honest RAM interpretation, preserved old failure, clean local commit | Pending |

## Measurement contract

- Keep the existing NBReq client probe source, manifest and lockfile unchanged. The new fixture is a separate executable/process with its own unpublished crate.
- Generate a temporary private CA and leaf key in Rust memory; persist only the public DER root needed by the client verifier. Bind an ephemeral IPv4 loopback port only.
- Explicitly select TLS1.2 or1.3; record the negotiated version for every completed handshake. Retain exactly 16 or32 connections until instructed to stop; cap handshake and whole-process duration.
- Python orchestrates and samples only the NBReq client PID. Fixture memory, certificate generation and the Python controller are excluded from client observations.
- Preserve client baseline/connecting/ready/closing/released phases. The ready phase must retain every connection; after drop, connection count and logical queue reservations must both return to zero.
- Preserve raw logs, per-phase samples, compiler/host identity, source manifest, binary hashes and command exit status. A fixture failure cannot count as a passing observation.
- Clean up all owned subprocesses on ordinary completion, timeout, partial launch or evidence-write failure. Never kill unrelated processes.
- RSS observations are whole-client-process measurements, not per-connection allocation accounting or a production ceiling. Mac RSS is not private memory/footprint. Logical admission reservations are reported separately from physical RAM.

## Working record

- **MF0:** root re-read the prior raw ARM failure and the accepted observer/probe contract. A separate Rust fixture avoids changing the client executable or relying on the host's Python SSL build. Existing phase windows remain two seconds baseline, five seconds held and two seconds released; no RSS pass/fail threshold is introduced.
- Team: `smtp_worker` owns fixture/harness/docs; `smtp_tests` owns independent new tests; `smtp_review` reviews read-only and directs corrections; root owns plan, bridges, frozen snapshots and measured evidence.
- The independent Sol test agent hit model capacity twice before writing tests. Root took ownership of the independent test files; the Sol implementation worker and read-only Astra reviewer continue their original roles.
- ARM and Linux readiness requests `20260925-222735-ed3b4f1f` / `20260925-222739-6deb2291` confirmed the expected architectures and exact original probe hashes. Free disk: ARM163GiB, Linux4.4GiB; only the small fixture needs a new build.
- `fixture-red.log`: compiled process tests for verified TLS1.2/16 and TLS1.3/32 both failed in0.05s when the intentional scaffold closed the connection before TLS. This is behavioral red evidence, not a compilation failure or timeout.
- Root supervisor review found duplicate-run evidence replacement and separate-process-group cleanup risks. An exclusive attempt token now precedes all result writes; fixture and client will share the observer's POSIX process group so an outer fallback reaches both. Three real no-network supervisor tests pass, including timeout and metadata-write failure reaping. The fixture's actual binary name is `tcp-tls-fixture`.
- **Outage recovery:** a Codex outage interrupted the initial implementation. Root inspected the persisted scaffold and logs before restarting the team as `fixture_worker` (Sol high), `fixture_tests` (Sol high), and `fixture_review` (Astra xhigh). Root retains ownership of the Rust process tests, plan and host/evidence helpers; the recovered test agent owns Python controller fault tests.
- Independent review strengthened accepted-socket tests: they must remain open before STOP and reach EOF/reset by terminal success, in addition to listener closure. The real TLS1.2/1.3, wrong-root, wrong-version, early-stop, slow-handshake and admission tests now pass.
- The reviewer reproduced Windows `taskkill` denial in its sandbox. Raw evidence is retained in `reviewer-helper-failure/`. The root supervisor now falls back to its owned process handle, treats cleanup logging as best-effort, and records terminal exit codes even after failure. Five Windows helper tests pass, with the real POSIX surviving-descendant case retained for remote runs. POSIX cleanup sends a final group KILL even if the leader exits during the TERM grace period.
- Independent controller faults exposed a late raw-log error incorrectly allowing a successful report. The worker now propagates reader failures, starts output capture before sampler construction, closes owned pipes, and publishes JSON atomically only after cleanup and sample persistence. Nine controller tests cover real subprocess cleanup, a successful fake-child control, unrelated-PID safety, bad metadata, partial launch, death/deadline, logging and publication failures. Fake-child controls are lifecycle evidence, not TLS evidence.
- Final independent local logs: `fixture-independent-tests.log` (seven real tests), `fixture-independent-msrv.log` (same seven on Rust1.85), `fixture-independent-clippy.log`, `fixture-independent-format.log`, and `observer-independent-tests.log` (nine tests). Astra inspected them and approved source freeze, conditional only on frozen native/POSIX evidence. Windows development smoke reports cover both protocols and both counts; those use a newly built development client and remain separate from ARM/Linux's unchanged `bede85c` probes.
- **First frozen run (`6462562`):** Windows passed the full gate. Both native POSIX supervisor suites passed, including a descendant that ignores TERM after its parent exits. ARM then exposed a test-only `/var` versus `/private/var` temporary-path comparison; Linux exposed the short interval in which process sampling reports disappearance before the child becomes waitable. Both remote gates stopped before measurements and remain preserved as failed attempts.
- The controller now permits at most 0.1 seconds to confirm an owned child's exit after the existing narrow sampling exceptions. A still-live child remains an error; no sample is invented. Test paths are canonicalized and subtest output directories remain independent after failure. The strengthened exit-race regression fails against the prior observer (`observer-sampling-exit-prefixed-red.log`); all 11 corrected tests pass (`observer-independent-portable-11-v2.log`). Astra approved the exact correction for a new source freeze. Production library and client-probe source, manifest and lockfile remain unchanged.

## References

- [TCP TLS plan and original evidence](nbreq_tcp_tls_plan.md)
- [SMTP development checkpoint](nbreq_smtp_plan.md)
- [Probe and memory observer](../tools/tcp-tls-probe/README.md)
