# M2.5 — GDS consuming response conversion

Verified 2026-09-08. Tracker: E-12 / MD-15. M2.5 completes the scoped M2 work.

The owner cleared GDS and explicitly accepted nbreq 0.2 with local builds. Its
manifest requires `=0.2.0`; its lock records the local graph without absolute paths.
The existing local helpers verify the selected checkout and use private locks.
They now permit ureq-only checks because Cargo still resolves optional dependencies.
Source descriptions no longer incorrectly report 0.1.0. Publication and a registry
lock refresh remain release work.

## Implemented contract

Typed synchronous replies, NBReq waiters and legacy text replies consume unique
response storage into the returned String. An unexpectedly shared body is explicitly
copied, preserving existing behavior and the other owner's data. Pointer/capacity
assertions prove the original allocation transfers, including empty/spare storage.
JSON response parsing already borrows bytes.

Typed byte limits are checked before extraction/copy and still precede UTF-8
validation. Typed non-2xx status/body remain available. Legacy text preserves
status-first rejection and detailed UTF-8 errors. Request bodies, transport policy,
24 MiB ceilings, Engine ownership, cancellation and joined shutdown are unchanged.
These limits are post-download; early network limits and aggregate accounting remain M3.

This removes one payload-sized allocation and copy from successful unique conversion.
It does not establish measured whole-GDS RAM savings, zero overhead or acceptance on
128/256 MB devices. Transferred Strings/decoded data still need GDS admission policy.

## Verification

| Gate | Result |
| --- | --- |
| Initial x86 GDS reds | Two intended pointer failures: unique and empty/spare typed conversions. |
| Expanded x86 red pass | Three intended allocation failures, including legacy text; three semantic companions pass. Old legacy conversion was extracted unchanged into a testable helper. |
| Final x86 HTTP / WebRPC | 14 adapter tests (eight new) and 74 WebRPC tests pass, including retry/cancellation/join coverage. |
| Ureq-only x86 | 2 HTTP and 73 WebRPC tests pass. |
| x86 debug DLLs | Native and ureq-only builds pass with SkipCopy; build cache restored to native. PE machine is i386. Installed DLL SHA256 unchanged. Existing warnings remain. |
| Linux exact adapter | 14 native and 2 ureq-only tests pass in a harness. Adapter hash matches Windows. This is not a full GDS Linux build. |
| Both Macs, stable and Rust 1.85 | Each of four runs passes 393 nbreq library and 5 Darwin-helper tests with Core Foundation 0.10.0. |

Companions cover shared ownership, empty/spare capacity, multibyte UTF-8 byte counts,
exact/over/zero/unset limits, invalid UTF-8 and non-2xx replies. Real loopback checks
exercise typed sync/waiter delivery and legacy text behavior.

## Dependency conflict and failed attempts

The first GDS attempt failed during resolution, before tests: serialport 4.8.1 pins
Core Foundation =0.10.0, while nbreq-darwin pinned =0.10.1. This blocks resolution
even on Windows. The only nbreq production change for M2.5 permits compatible 0.10
patch versions. Its ordinary lock still selects 0.10.1; GDS selects 0.10.0.
GDS's serial-port code/dependency was unchanged. The lower patch passes both Mac
toolchains; M2.4 already verified 0.10.1.

First Mac offline attempts lacked the older patch; after fetching it, unchanged
source passed. Missing-cache logs are retained. An initial evidence export expected
a separate helper lock; the helper uses the root workspace lock. Corrected exports
are complete. Linux readiness probes needed an absolute Cargo path and corrected
quoting. These were setup issues, not test failures. All remote jobs have finished.

## Durable source and evidence

- nbreq checkpoint: `2c382b7`, after `2f89bef` recorded M0–M2.4. The
  [frozen Darwin source](evidence/nbreq-m25-darwin-20260908.tar.gz) uses M2.4 B
  production/tests plus the manifest range change. SHA256:
  `c80bbed011871946810cab284251f7edfdf0c4d7f9165ee62ec38fd00321e7d8`.
- [Mac evidence](evidence/nbreq-m25-darwin-evidence-20260908.tar.gz) and
  [artifact identities](evidence/nbreq_m25_artifacts.json) preserve full logs,
  source/lock checks and missing-cache attempts.
- GDS baseline HEAD: `48048a75f8199dbbb3116b15d6fcd6ed8012949b`. Its report is
  `C:/User/SecuritasNew/gds/doc/nbreq_m25_conversion.md`. Its private durable
  `gds/doc/evidence/nbreq-m25-20260908.tar.gz` contains 89 files: six-file
  before/final snapshots, red source, locks, full logs and the Linux harness.
  SHA256: `96c6f76f63833d038a57923f42b1362da80a26f1b006d89935d1ecbdad00a517`.
- Final GDS adapter SHA256:
  `2efc09dc123ce602217999d8ca3af85950ca372786f5fb8e0332528c2bc5e45f`.
  Private GDS source/evidence stays in its own repository. A stale pre-existing
  test-output file is labelled and excluded from the durable archive.

Unrelated GDS WAL/optimiser work and older nbreq F5 edits remain outside this item.
Nothing was published/deployed, and no GDS process was started or stopped.
Next: resolve MQ-03 before M3 early limits and aggregate budgeting.
