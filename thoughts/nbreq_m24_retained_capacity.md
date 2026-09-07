# E-11: M2.4 retained connection capacity

Date: 2026-09-08. **M2.4 verified on Windows x64/x86, Linux x64, Intel Mac and Apple Silicon Mac.** Companion to the
[memory tracker](nbreq_memory_plan.md) and [M2.3 measurements](nbreq_m23_request_ownership.md).

## Scope and findings

The M2.3 5.9 MiB measurement deliberately holds a 4 MiB reply. That payload remains legitimate
application-visible data. After releasing it, the extended workload records about 1.87 MiB
Windows / 1.95 MiB Linux live heap. This is a mixed-workload endpoint, not a pure before/after
large-request comparison: preceding phases replace connections and exercise streaming readers.

Two concrete retained allocations are in scope:

1. `NativeReactor` drains its send VecDeque but preserves capacity when HTTP parks a clean
   connection. Large TLS uploads can leave a 512 KiB queue on an idle socket.
2. `NativeTls::consume_retained_plaintext` clears the fully consumed streaming plaintext Vec.
   The next `retain_stream_progress` replaces that allocation with its incoming Vec, so keeping
   the empty allocation provides no reuse. Partial unread plaintext must remain intact.

Implemented policy: release consumed streaming plaintext immediately. When a completed, reusable HTTP
connection is parked, trim send capacity only above 128 KiB, targeting 64 KiB. Preserve smaller
queues as-is and leave active/unsent data untouched. The gap avoids repeated resizing around the
normal 64 KiB transmission window plus headers. Keep the connection/TLS session and configured
transmission limits. This is internal retention policy, not a new payload/admission ceiling.

Do not shrink returned response bodies, discard valid unread bytes, replace TLS, add pools or
change GDS in this item. M2.5/M3 remain separate.

## Red/green proof

Starting checkout matches the final M2.3 D source exactly (manifest
`1c233f6a2627a1aea6383e6365d1a779ed5a6d2016eb31ba6321d5167e0afa62`). Existing dirty work is preserved.
Three initial tests failed at the intended capacity assertions: buffered and streaming TLS uploads
each left `[524288, 524288]` bytes of send storage after a 2 MiB then a 50 KiB upload on the same
socket; the TLS unit test preserved partial bytes but retained 69,632 bytes of empty capacity.
The original failing source and `retention-red.log` are retained under `target/m24`.

All three are now green. Three additional native-reactor tests cover threshold boundaries,
unchanged small-buffer allocation/pointers, a normal 50 KiB request after trimming, preserved
transmission limits, rejection of queued output without losing any bytes, and refusal to park
connecting, half-closed or stale sockets. The TLS test additionally covers invalid consumption,
idempotent empty consumption and the next plaintext window. Six tests and the lint preflight pass.

## First wider failure and fixture correction

The A full verifiers failed the existing `idle_peer_close_is_evicted_before_the_next_request`
assertion on all four hosts: Windows default tests (396 pass / 1 fail), Linux native-only
(342 / 1), each Mac native-only (340 / 1). The fixture closed its socket immediately after
sending the reply; the new parking guard can correctly reject an already half-closed socket
before it ever enters the idle pool. Both requests succeeded on replacement connections, but
that path does not increment the idle-eviction counter. This is not proof of failed eviction.

B changes only that test: the server waits for the client to observe one parked connection
before closing; the client waits with a deadline for eviction before submitting the next request.
The original replacement/no-reuse/eviction assertions remain. Its focused Windows check passes.
The A source and all failed logs remain preserved. No production change was needed after A.

## Final source and completed gates

Snapshot: `nbreq-m24-retention-20260908-b`, 120 hashed files plus manifest. Five files differ from
M2.3 D: `src/backend/native.rs`, `src/backend/native_http.rs`, `src/backend/native_http/tests.rs`,
`src/backend/native_http/tests/retained_capacity.rs`, `src/backend/native_tls.rs`.

- Archive SHA-256: `9d89ffe745b0f620569c3234edf658f48e15f85cf388399ff95997678cf11f00`.
- Manifest SHA-256: `b1ddee56304a55f26f9ef0c7b215e9ca1d5f10632c47aa5bcd764965b15b519d`.
- Working logs/source: `target/m24/b`; the final checkout matches the frozen B source. Superseded A is
  retained under `target/m24` (manifest `d3103ccecef5a64b6878cafb7bd5c15eeaf76b10062789464f0a30a3cc87b887`).
- Windows pipeline `windows.ps1`: full verifier, 29 M2 x86 tests, release plain/meter binaries,
  then 60 paired cases against frozen `target/m23/c/bin` with checked identities.
- Linux lab `/home/ubuntu/nbreq-m24-retention-20260908-b`: stable/MSRV full verifiers, then
  60 paired cases against `/home/ubuntu/nbreq-m23-request-20260907-c/bin`.
- Intel lab `/Users/andrew/nbreq-m24-retention-20260908-b`: stable/MSRV full verifiers.
- ARM lab `/Users/m1/nbreq-m24-retention-20260908-b`: stable/MSRV full verifiers.

Bridge receipts are `bridge-probes.json`, `bridge-uploads.json`, `bridge-launches.json`. Uploads
were checksummed before extraction. Remote runners use fresh xtask build directories and clear
only NBReq package artifacts in the established M1 dependency cache to avoid stale relocated builds.
All gates satisfy current-root/new-test markers, successful process exit and joined observers.
Windows passes 24/24 steps and 29 x86 M2 tests. Linux and both Macs each pass stable and Rust
1.85.0 full verifiers: seven full runs in total. All jobs and observer processes have finished.
No test failure remains unresolved. A failed read-only evidence-collector command had quoting
errors; corrected collection preserved the original failed A logs unchanged.

## Final paired measurements

Each host ran three alternating before/after repetitions of HTTP/HTTPS 1 KiB and 50 KiB plus
extended HTTPS 50 KiB: 32 connections, 16 steady rounds, separate fixture/client processes and
separate plain/meter binaries. All 120 cases validate exact bytes, accounting, reuse, TLS trust
where applicable, and joined cleanup. Before is frozen M2.3 C (production identical to D);
after is frozen M2.4 B. Heap endpoints below are medians; peaks are maxima over three cases.
Process memory is sampled from uninstrumented runs and uses different OS counters.

| Extended HTTPS checkpoint | Windows before → after MiB | Linux before → after MiB |
| --- | ---: | ---: |
| Ordinary burst released | 2.401 → 2.401 | 2.475 → 2.474 |
| Slow readers drained | 1.380 → 0.882 | 1.451 → 0.951 |
| 4 MiB reply still held | 5.880 → 4.944 | 5.950 → 5.012 |
| Large reply released | 1.880 → 0.944 | 1.950 → 1.012 |
| After joined shutdown | 0.014 → 0.014 | 0.010 → 0.010 |

The post-large endpoint follows cancellation/replacement and streaming phases. It is not a
pure single-upload experiment. The deterministic connection tests separately prove 512 KiB
send storage is trimmed on idle parking and the same connection handles the next 50 KiB upload.
The held 4 MiB reply remains legitimate application data. Heap release does not require the
process allocator to return pages to the OS immediately.

| Workload | Windows allocated KiB/request before → after | Linux allocated KiB/request before → after |
| --- | ---: | ---: |
| HTTP 1 KiB | 8.33 → 8.14 | 8.16 → 8.16 |
| HTTP 50 KiB | 230.97 → 230.91 | 232.59 → 232.41 |
| HTTPS 1 KiB | 12.72 → 12.79 | 12.71 → 12.71 |
| HTTPS 50 KiB | 539.10 → 538.42 | 539.40 → 539.31 |
| HTTPS 50 KiB extended | 538.65 → 538.67 | 539.21 → 539.17 |

Ordinary steady allocation churn and small-call idle retention are essentially unchanged.
The change primarily reduces unused storage retained after streaming/large work. It does not
replace M3 admission/accounting or establish a whole-GDS/128–256 MB device budget.

| Observed extended-workload peak | Windows before → after MiB | Linux before → after MiB |
| --- | ---: | ---: |
| Live Rust heap (meter) | 11.78 → 11.51 | 11.95 → 11.47 |
| Working set / RSS (plain) | 29.36 → 27.82 | 23.95 → 22.82 |
| Private commit (Windows only) | 20.00 → 18.47 | Unavailable |

## Performance and limits

| Short batch median throughput (requests/s) | Windows before → after | Linux before → after |
| --- | ---: | ---: |
| HTTP 1 KiB | 30614 → 34666 | 14721 → 15787 |
| HTTP 50 KiB | 6291 → 5581 | 1304 → 1312 |
| HTTPS 1 KiB | 33154 → 27413 | 12485 → 13220 |
| HTTPS 50 KiB | 4715 → 4972 | 1193 → 1194 |
| HTTPS 50 KiB extended | 4665 → 2710 | 1184 → 1215 |

The short Windows batch contains inconsistent throughput/latency, including material apparent
slowdowns. One predeclared follow-up used the same frozen plain binaries with 32 connections,
128 rounds and five alternating pairs for each of three workloads: 30 valid cases, all joined.
It is a separate longer workload; preserve both batches rather than replacing the original data.

| Longer Windows run | Median requests/s before → after | Median p95 ms before → after |
| --- | ---: | ---: |
| HTTP 50 KiB | 6039.91 → 5917.03 | 4.46 → 4.84 |
| HTTPS 1 KiB | 31079.47 → 30745.65 | 1.20 → 1.33 |
| HTTPS 50 KiB | 4704.19 → 4929.22 | 7.18 → 5.97 |

Longer-run median throughput changes approximately -2.0%, -1.1%, +4.8%; median p95 latency
changes +8.4%, +10.2%, -16.9%. Ranges overlap and these hosts are not isolated benchmark machines.
The evidence supports proceeding, but does not establish zero cost or exclude smaller latency
regressions. Full ranges and process/phase counters are in the archived summaries/raw records.
Cross-host timing differences are not attributed to platform software.

Repeated large uploads may regrow/retrim the send queue; this is the deliberate memory/reuse
tradeoff. There is no dedicated sustained-large throughput study in M2.4. Remaining stream/TLS
state, native allocations, allocator pages, application-held bodies and GDS queues remain real
costs. M2.5 is next; MQ-03 must precede M3 budgets, and actual-device acceptance remains M4.

## Durable evidence and next step

- [Final B source](evidence/nbreq-m24-retention-20260908-b.tar.gz) and
  [superseded A source](evidence/nbreq-m24-retention-20260908-a.tar.gz).
- [Evidence archive](evidence/nbreq-m24-retention-evidence-20260908.tar.gz): 1,049 files including
  initial red source/logs, all failed A full runs, final B verifiers, x86 companions, raw paired
  cases, longer timing cases, summaries, build provenance, source differences and bridge receipts.
- [Artifact identities](evidence/nbreq_m24_retention_artifacts.json) records source and evidence
  checksums. Evidence SHA-256:
  `7baf68213a03cce00cd25fe10a44deea110bcdf6ab9808420bcdace0425302ca`.
  Archive contents were read back and checked against their per-file manifest.

All final checks passed; no running work remains. Existing dirty changes were preserved and no
commit/publication or GDS source change was made. Next is M2.5: inspect GDS's current version/build
workflow, then apply the light consuming-response conversion within its existing status, size and
UTF-8 rules. Broader consumer work remains in the [GDS handoff](gds_memory_handoff.md).

Subsequent checkpoint preparation removes one trailing blank line from
`tools/f5-observe/memory/meter/Cargo.toml`; its parsed TOML is identical. Runtime/test source and
the frozen archives are unchanged. The earlier F5 registry-comparison edits remain outside the
review/memory checkpoint commit.
