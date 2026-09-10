# M4.1 — GDS workload admission

2026-09-08. Verified for the scope below; M4 installation/device acceptance remains open.
The implementation is in GDS. nbreq's final M3 E source is unchanged, with all 119 ordinary
source-manifest inputs rechecked. Private GDS source and logs remain in its repository.

## Result

GDS now slows incoming WebRPC work when queued bytes or outgoing replies accumulate. Local
pressure slows the affected peer; shared pressure slows all production peers. Lower resume
thresholds let backlogs drain before polling restarts. Queue capacity remains accounted for
through successor-poller handoff and expires with abandoned handoffs.

Outbound work reserves pipeline allowance before Rust formatting/encryption, shrinks after
encoding, and retains its allowance through queueing and retries. Large frames cannot occupy
all four POST workers or consume the protected small-frame byte reserve. Eligible waiting
pollers take turns, and another origin can proceed when the first origin is saturated.

Initial defaults are configurable: inbound pause/resume at 32/16 MiB shared and 8/4 MiB per
peer; a 128 MiB outbound reservation allowance with 8 MiB protected for small frames; three
large deliveries; 16 active polls and four per origin. Native HTTP controls now expose 32 total
sockets, eight per origin and 64 accepted requests, down from the inherited 1,024 accepted
requests. Startup checks leave capacity beyond polls and large deliveries for small work.

These allowances are not upfront allocations. The existing 24 MiB per-body ceilings remain,
and the nbreq aggregate body-capacity cap stays opt-in. No automatic eviction or response replay
was introduced. An outbound admission failure means the response was not accepted; existing
Delphi handling logs that failure. No DLL was installed and no commit was made by this step.

## Verification and performance

Seven actual runtime reds were preserved before fixes. Final GDS production B passes Windows
x86 native HTTP/WebRPC 17/90 and ureq-only 2/89 tests; both DLL configurations build with local
nbreq and SkipCopy. Nine portable policy tests pass on Windows x86 and Linux stable/Rust 1.85.0.
This Linux check covers the exact admission module, not the full GDS DLL integration.

Corrected component measurement C uses B's unchanged admission source and one wall clock
across workers. A/B's individual-worker timing could undercount elapsed time; those results
are retained as superseded evidence. The extra reservation bookkeeping costs about 0.048
microseconds per operation on Windows x86 and 0.035 microseconds on Linux x64 at one worker.
At eight workers, added elapsed time divided by total operations is 0.274 and 0.035 microseconds
respectively. Seven alternating repeats were used; these are component measurements on
different hosts, not end-to-end request latency or a platform ranking.

Small-work protection preserves worker/byte headroom against large deliveries and orders eligible
pollers. It cannot know an incoming request's size before polling. Slow servers, all-small overload,
or unrelated HTTP consumers can still delay small requests. Four same-origin polls may increase
pickup latency, particularly with long polls; tune that limit with socket capacity.

## Evidence and next step

[Artifact identities](evidence/nbreq_m4_artifacts.json) reference the private GDS archive
`C:/User/SecuritasNew/gds/doc/evidence/nbreq-m4-admission-20260908.tar.gz`. It preserves changed
source before/A/B, C measurement source, all red/green and build logs, temporary test manifests,
raw CSVs, checksums and analysis. Full controls and ownership details are in GDS's
`gds/doc/nbreq_m4_admission.md` and `gds/doc/nbreq_memory_controls.md`.

Next measure representative GDS installations: mixed small/large work, idle and slow peers,
same-origin bursts, pickup latency, refusal counts and memory retained in Delphi. Soft inbound
watermarks allow already active replies/decoding to overshoot; the outbound allowance is not
a whole-process cap. Choose actual installation headroom and any nbreq aggregate cap from that
workload. A 128/256 MB device profile is not established by these component checks.
