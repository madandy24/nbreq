# R5 examples refresh

Opened 2026-09-15. Status: implemented, verified and committed; integrated main `451a769` is pushed. Owner approved the A/B/C learning
sequence and removal of superseded examples, with an explicit cancellation demonstration.

## Resume checkpoint

| Field | State |
| --- | --- |
| Workspace | Implemented in R5 and committed as `655a6e3`; reconciled main `451a769` is pushed. Use [housekeeping](nbreq_housekeeping.md) and the release plan for current source and verification identities. |
| Scope | 17 complete programs: A01–A11 HTTP, B01–B03 DNS, C01–C03 TCP. HTTP cancellation is A07, ahead of manual/streaming/budgets/owner lifecycle. |
| Endpoints | HTTP defaults to httpbin HTTPS GET/POST, DNS to example.com. Destinations are overridable. Cancellation and default TCP use bounded local fixtures. |
| Teaching rules | One main idea per example; real public NBReq calls visible in each program. Short finite deadlines, verified TLS, clear shutdown, no Tokio or implied async/await API. Shared support contains server plumbing only. |
| Cancellation proof | Local server acknowledges receipt and withholds the response. Cancel the handle; require the canonical `Completion::Cancelled`, not merely a successful cancel command. |
| Validation | Build all examples on stable and Rust 1.85, native-only feature build, lint/format/doctests, local HTTP/TCP/cancellation execution, separately labelled live HTTPS/DNS smoke. Check packaged sources and links; automate executable examples in CI. |
| Remaining | README presentation, main integration/push and final local packages/consumers are complete. [Housekeeping](nbreq_housekeeping.md) records the updated hosted run. Helper publication and registry-only proof remain separate release gates. |

The old examples may be removed after replacement; Git retains their history. Historical reports
and sealed evidence continue to describe their original seven-example inventory.

## Result and verification

- Added the [17-program index](../examples/README.md), registered all programs and packaged both
  local server helpers. Removed all nine old example files, including the two unregistered WP9
  comparisons. README/guide links and illustrative POST endpoints now match the new sequence.
- Windows x64 stable and Rust 1.85 build all 17 programs and pass the 16 local execution cases
  each: 11 HTTP programs, three TCP programs, HTTP 404 handling and response-limit rejection.
- All three DNS programs pass live system-configured lookups of example.com. A01 passes verified
  HTTPS GET at httpbin; all three POST programs pass against their default httpbin endpoint,
  including the chunked upload. These are live-network observations, separate from local fixtures.
- `cargo fmt --all --check`, warning-denied all-target/all-feature Clippy, native-only example
  build, minimal-feature target selection, stable/MSRV doctests and the existing package inventory
  test pass. No runtime source or dependency versions changed in this slice.
- Cargo packages and verifies the working candidate with explicit local helper overrides. The
  normalized archive contains exactly 17 example sources, two helper sources and the index; all
  relative Markdown links resolve. All examples build from an unchanged archive unpacked outside
  the source workspace, and its 16 local execution cases pass. This dirty-source rehearsal is
  not the final release artifact and does not establish registry-only installation.
- The cancellation negative control is a private copy with only the cancel statement removed.
  It exits unsuccessfully with `expected Cancelled, got TimedOut`; the ordinary example verifies
  `Completion::Cancelled`. The production example source was never mutated for this check.
- CI now builds and runs the local examples in each existing platform/toolchain job and retains
  logs/results. The release-consumer runner uses the same harness and accepts both helper archives,
  including the Wine-fix winpoll 0.1.1. Updated CI has not been dispatched in this slice; Windows
  results do not claim Linux/macOS execution of the refreshed examples.

Two validation setup failures were corrected without changing NBReq: Windows accepted sockets
in the new fixtures inherited nonblocking mode, so the helpers now explicitly select blocking I/O;
and building Cargo's normalized package beneath the source workspace triggered workspace discovery,
so the identical archive was unpacked into an independent temporary directory. Original logs remain.

Local logs are under `target/worktrees/nbreq-r5/target/examples-check-*`,
`examples-live-post` and `examples-final-checks`. A compact durable
[evidence record](evidence/nbreq_examples_refresh_20260915.json) retains inputs, outcomes and logs
for the cancellation proof. Existing sealed R5 evidence and its pending public-upload approval
remain unchanged. No merge, push, tag, crate publication or GDS change was performed.
