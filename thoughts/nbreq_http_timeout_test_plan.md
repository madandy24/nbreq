# HTTP timeout test stability

Opened 2026-09-27. Narrow release-preparation follow-up to the Windows timing
qualification in [portable TLS acceptance](nbreq_portable_tls_plan.md).

## Checkpoint

- Owner requested fixing the timing-sensitive test before continuing release work.
- Working checkout: `target/worktrees/nbreq-smtp`, branch `codex/nbreq-smtp`, starting
  at `2375ee0`. Main remains the published 0.2.0 baseline.
- Sol high worker owns the focused test/fixture repair; Astra xhigh independently
  reviews design, changes and raw evidence. Root owns this record and final scope.
- Active stage: complete. Independent review accepted the design, two-file test
  repair, raw results, documentation and every archived member with no remaining
  actionable finding.
- Evidence lab: `C:/User/projects/nbreq/target/http-timeout-test-20260927/`.

## Problem and acceptance

`total_and_inactivity_timeouts_close_the_stalled_socket` starts a 150 ms request
clock at admission, then requires a fixture event emitted only after a complete
request and response head. A valid timeout can precede that event. The original
Windows bundle-only run established the timeout category but failed the later
event expectation; its exact iteration and socket phase remain unknown.

The repair must preserve meaningful total/inactivity classification and actual
socket-closure evidence. Separate timeout-before-network from established
response-stall coverage where necessary. Use explicit phase control and bounded
fixture cleanup; increasing a timeout, serializing the whole suite, or ignoring
missing events alone does not resolve the faulty assumption. Inspect the related
native unit test for the same assumption without broad unrelated test changes.

## Steps

| ID | Work | State |
| --- | --- | --- |
| HT1 | Review design and capture a compiling, controlled reproduction of the invalid phase assumption | Complete: delayed manual owner correctly returns Total, then old RequestSeen assertion fails |
| HT2 | Repair focused tests/fixtures; preserve timeout and cleanup assertions | Complete; independent code/design review accepted |
| HT3 | Run affected feature/MSRV checks, lint and bounded scheduling repetitions; independently review raw evidence | Complete; all checks passed and independent raw-evidence review accepted |
| HT4 | Record exact scope, residual qualifications and accepted checkpoint | Complete; documentation, archive and index independently verified |

Production timeout behavior, TLS implementation, dependency versions, publication
and GDS integration are outside this repair unless a demonstrated defect requires
an explicit scope update. Preserve historical failures rather than relabeling
them as successful runs.

## Design and initial evidence

The controlled red switches only the target test to manual driving and delays its
first drive by 200 ms, beyond its 150 ms total deadline. Compilation succeeds;
timeout category assertions pass; the original request-arrival expectation fails.
This proves the invalid assumption, not the exact scheduling path of the historical
Windows incident. Raw red and historical logs remain separate in the evidence lab.

The repaired integration test drives the native Engine and loopback peer from one
thread. A fresh empty GET has zero buffered-body reservation; parsing a response
head with Content-Length 1 reserves exactly one byte after temporary input staging
is released. That metric plus a pending request proves an established body stall.
The peer sends no body or further bytes. Only then does the test pause driving
through the selected real deadline, assert the exact failure category, check budget
release, and observe EOF/reset at the peer before Engine shutdown. Setup and all I/O
remain bounded; ordinary OS scheduling headroom remains necessary.

The related native unit test retains spawned-owner timeout classification and
fixture cleanup, without demanding a particular network phase or a sub-500 ms
scheduler result. Existing before-first-drive/no-connection coverage is retained.
Together these tests distinguish admission expiry, spawned classification, and
mandatory established-response-stall closure.

Review also removed the spawned smoke test's competing two-second total deadline
from its inactivity arm. It now configures only the clock it is testing and bounds
the caller's wait separately with `wait_for`. Phase assertions identify the backend
and selected clock. Neither change alters production timeout behavior.

## Final verification and evidence

Native Windows x86_64, stable Rust 1.97.1 and minimum Rust 1.85.0:

- Complete `http_adversarial`: 16/16 each under stable default, native-only,
  bundle-only and all-feature configurations; 16/16 under Rust 1.85 all features.
- Four relevant unit tests pass individually on both compilers: spawned timeout
  classification, total expiry before the first drive, unused-fixture cleanup,
  and incomplete-request cleanup.
- All-feature/all-target strict Clippy and formatting pass.
- Two waves of two overlapping all-feature integration executables pass all 16
  tests in every run (64 additional executions). No serial-test override is used.
- Source hashes are checked before/after verification and repetition; the repeated
  executable's hash is also unchanged. All 17 verification commands pass.

The controlled red is a successful compilation followed by the expected failed
request-arrival assertion. The final focused green and broader evidence use the
repaired source. The original Windows bundle-only failure remains retained and is
not reclassified. The old incident's exact timeout arm/network phase remains
unknown. This follow-up verifies the test repair locally; it does not claim a new
remote-platform or Wine run. Finite one-second setup and two-second clock/I/O
headroom remain, with explicit phase diagnostics rather than an injected clock.

Retained [evidence archive](evidence/nbreq-http-timeout-test-20260927.tar.gz):
87 regular files, 167,944 bytes, SHA256
`e1d8230bf020269b8f55ba729e9a133a3d2c7ace5c354f374d7ae2eafde8ad23`.
The [artifact index](evidence/nbreq_http_timeout_test_artifacts.json) records each
member's length/hash, the tested source identities, compiler/command records and
results. Sources, original/controlled failure logs and green checks are included;
compiled executables remain in the local evidence lab.

Only two test files change. Production code, manifests and locks are unchanged.
No merge, push, publication or GDS change is part of this checkpoint.
