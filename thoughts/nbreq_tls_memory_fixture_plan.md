# TLS memory fixture follow-up

Opened 2026-09-26 NZST. This closes the optional ARM memory-observation gap without changing NBReq's production TCP/TLS implementation.

## Resume checkpoint

| Field | State |
| --- | --- |
| Authority | Owner approved replacing the Python TLS test server with Rust, then repeating the missing ARM observations. Existing Sol-worker/Astra-reviewer cycle continues. |
| Working branch | `codex/nbreq-smtp`, worktree `C:/User/projects/nbreq/target/worktrees/nbreq-smtp`, baseline `3f841be`. This branch includes the accepted TLS and SMTP work. Main and the earlier TLS branch remain unchanged. |
| Scope | Separate unpublished Rust loopback TLS server, Python process sampler/orchestrator, focused lifecycle/protocol tests, native measurements and durable evidence. No production library changes, mail submission or host runtime installation. |
| Active stage | MF0–MF4 complete. Windows, ARM and Linux passed at fixture/controller candidate `f35aa6e`; Astra accepted source, raw evidence, final documentation and the permanent archive with no open findings. Earlier failed attempts remain preserved and separately labeled. |
| Evidence lab | `C:/User/projects/nbreq/target/tls-memory-fixture-20260926/`. Preserve failed development checks as such, source and binary hashes, exact commands, raw process samples and actual negotiated protocols. |
| Baseline client | Existing `bede85c` release probe on ARM/Linux, unchanged source and executable where present. ARM SHA256 `76e9f6483bea26156dd85b91708ea3e221c6037e2e8a34d1acaf98cc17e187e5`; Linux SHA256 `f43abdad13da6594a7019f36c553d6c68c21affd7169b086e9b28e435fb33c98`. |
| ARM outcome | The unchanged original client now passes both TLS versions and connection counts with the Rust fixture. The prior Python3.9/LibreSSL2.8.3 server failed with `NO_SHARED_CIPHER`, including against an independent LibreSSL client. The memory-observation gap is closed; exact internal LibreSSL cause remains unknown. |

## Stages

| ID | Work | Acceptance | Status |
| --- | --- | --- | --- |
| MF0 | Fixture and measurement contract | Explicit versions/counts, loopback and deadlines, subprocess ownership, unchanged sampled client | Accepted |
| MF1 | Rust fixture and observer replacement | Real verified TLS1.2/1.3, no Python SSL/OpenSSL dependency, bounded deterministic lifecycle | Implemented; seven independent Rust process tests passed |
| MF2 | Independent checks and review | Worker fixes findings; wrong-root/protocol/failure cleanup coverage; strict lint and Rust1.85 | Complete; Astra approved final source and inspected all independent local logs |
| MF3 | Frozen native observations | ARM 16/32 connections for explicit TLS1.2 and1.3; matched Linux and Windows checks as useful; source/client identities retained | Complete: all three gates passed; original ARM/Linux client hashes unchanged |
| MF4 | Evidence and handoff | Reviewer accepts raw observations, honest RAM interpretation, preserved old failure, clean local commit | Complete: Astra accepted the aggregate and docs; this documentation/evidence checkpoint closes the follow-up |

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

## Frozen native evidence

Fixture/controller commit `f35aa6e02fb4acf003716d51863ad8c27d793321`. The source manifest SHA256 is `2d34189d469c38535aa95e5b5e1bd2163fa6e7d01477ee8d88ef05ec53ee01f2`; source archive SHA256 is `bae567a31d9329fa895700daa88190215c24f7b37f5f5524d1f37cff4e02f525` (11 source/helper files plus manifest, 25,847 bytes). Source and client identities were checked before and after each run. Production NBReq/SMTP and the existing client-probe source, manifest and lockfile have no changes from `3f841be`.

| Host | Execution evidence | Role |
| --- | --- | --- |
| Apple Silicon Mac | macOS26.6.1 arm64, Python3.9.6, Rust1.98 stable and1.85; complete gate passed in95.127s. Seven real fixture tests on each compiler, 11 controller tests, POSIX supervisor tests and strict Clippy passed. Both TLS versions and16/32 observations passed. | Closes the missing original-client ARM observation. Raw `arm-f35aa6e02fb4/`; retrieved archive SHA256 `6419fa2dfed4145988f4d8b1a789e7da2d82fb8f5e24e9de78fb677604f72f60`. |
| Linux | Linux5.4 x86_64, Python3.8.10, Rust1.98 stable and1.85; same checks and observations passed in229.607s. | Matched original-client reference. Raw `linux-f35aa6e02fb4/`; retrieved archive SHA256 `39557608b0ee19c3ef55aa65e6ac27a902ab6748135cc8bdd85f1698101902da`. |
| Windows | Windows11 AMD64, Python3.12.14, Rust1.97.1 stable and1.85; same fixture/controller/lint checks and observations passed in112.313s. Windows supervisor tests passed; POSIX-specific test skipped. | Harness validation with the retained historical development client SHA256 `43a9cd116a2535775dac5152a0ecd8e362d399d40b30a51473a2c86f6322016c`, not a new final-production-client claim. Raw `windows-f35aa6e02fb4/`. |

Every run records verified actual TLS1.2 or TLS1.3 negotiation, 16 or32 successful handshakes, a five-second held phase, and exact logical queue reservations of4,718,592 or9,437,184 bytes. The closing phase still owns all connections and reservations; released reports both zero. The two POSIX-only runs also prove cleanup reaches a descendant that ignores TERM after its parent exits.

Selected whole-client RSS medians, in bytes; all four observations per host and additional metrics remain in the raw archive:

| Host | TLS | Connections | Baseline | Held | Released |
| --- | --- | --- | ---: | ---: | ---: |
| ARM | 1.2 | 16 | 7,749,632 | 10,682,368 | 10,682,368 |
| ARM | 1.2 | 32 | 7,749,632 | 11,173,888 | 11,173,888 |
| ARM | 1.3 | 16 | 7,749,632 | 10,895,360 | 10,895,360 |
| ARM | 1.3 | 32 | 7,749,632 | 11,485,184 | 11,485,184 |
| Linux | 1.2 | 32 | 4,206,592 | 5,234,688 | 5,431,296 |
| Linux | 1.3 | 32 | 4,091,904 | 5,390,336 | 5,496,832 |
| Windows development client | 1.2 | 32 | 9,670,656 | 16,265,216 | 14,004,224 |
| Windows development client | 1.3 | 32 | 9,687,040 | 16,297,984 | 14,151,680 |

ARM held phases contain72–73 samples; Linux98–99; Windows99–100. ARM32 held RSS is about10.66MiB for TLS1.2 and10.95MiB for TLS1.3, versus7.39MiB baseline. Its reported CPU delta during the held window is about0.04s. These are idle loopback observations with16KiB application send/receive windows, not throughput tests, per-connection allocation estimates, cross-host performance rankings or a process ceiling. Mac RSS is neither private memory nor footprint. RSS need not immediately return to baseline when connections and logical reservations are released; these short observations do not establish long-term retention or leak behavior.

The earlier ARM/Linux `6462562` attempts remain failed-before-measurement evidence: archive SHAs `fec7a7b019325f91bbcad957900d9b8f450f633aaf182f9c277b2cc6632be397` and `364695b94112a226fb544d6c841eea2de9d4e614bc26e0346db667423e16bff6`. No main-tree merge, publication, mail submission or live external-service probe occurred in this follow-up.

## Durable evidence and review

- [Evidence archive](evidence/nbreq-tls-memory-fixture-20260926.tar.gz): 148,000 bytes, SHA256 `1895401ce2e10ae99cf383731d44e1bf4f4307c547192b186b76fcda290b17c6`.
- [Artifact index](evidence/nbreq_tls_memory_fixture_artifacts.json): 267 source/evidence/helper entries plus the embedded manifest; 736,036 uncompressed input bytes. Includes both exact source snapshots, three final native gates, two labeled earlier failed gates, raw samples, compiler and command records, development failures, regression proofs and bridge receipts. Excludes executables, build trees, private keys, credentials and GDS source.
- Astra independently recomputed raw summaries and checked phase/count/protocol accounting and original client identities on all three hosts. Remote manifests, archive members and bridge-returned archive bytes matched. The final documentation's numerical values and scope claims were accepted.
- Final review also strengthened the archive helper: final passed gates must match the selected commit/source; historical failed gates retain their own source identity; retrieved manifests must match their files and embedded result. Focused pre-fix checks demonstrated three false acceptances, retained as a transcript of the original tool output in `bundle-integrity-prefixed-red.log`; the corrected helper passed five tests in the directly captured `bundle-integrity-green.log`. The packager's corrected code and proof were independently accepted before the archive was created.
- Final aggregate review verified every member digest and length against both the index and original lab bytes, both nested source snapshots, all three final gate identities, both historical failed identities and all ten bridge receipts. Astra granted final signoff with no open finding. Main remains clean at `6ada1167`; the earlier TLS checkout remains clean at `c6872493`. This branch is ready for owner review of the completed TLS/SMTP development work and subsequent release coordination.

## References

- [TCP TLS plan and original evidence](nbreq_tcp_tls_plan.md)
- [SMTP development checkpoint](nbreq_smtp_plan.md)
- [Probe and memory observer](../tools/tcp-tls-probe/README.md)
