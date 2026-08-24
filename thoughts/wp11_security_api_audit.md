# WP11.2 security and API audit

Status: accepted on Windows and exact-source Ubuntu 20.04 / Rust 1.85 on 2026-08-24.

## 1. Audit scope

This pass reviews the public root crate plus the crates.io-required `nbreq-winpoll` support crate.
It covers the unsafe boundary, panic containment, diagnostic data, TLS policy, denial-of-service
bounds, pre-1.0 semver shape, MSRV compatibility, and vulnerability-reporting policy. It does not
claim that the unpublished repository has CI or a live private-reporting route yet.

## 2. Unsafe boundary

- NBReq proper retains `unsafe_code = "forbid"`; no root `src/` file contains an unsafe block.
- `nbreq-winpoll` contains the complete Windows compatibility FFI surface: one `WSAPoll` call and
  one argument-free `WSAGetLastError` call. The safe wrapper converts the target count to `u32`,
  owns a live writable descriptor array for the duration of the call, retains no pointer or socket,
  and documents both safety cases at the call sites.
- The support crate denies unsafe operations in unsafe functions. Its public API remains an
  implementation detail rather than a separately supported networking abstraction.

No additional unsafe escape hatch is accepted for DNS, TLS, HTTP, the reactor, callbacks, or FFI
consumers.

## 3. Panic and callback containment

- User callbacks run behind `catch_unwind`; a panic increments the callback-panic metric, releases
  the active request, and lets the worker continue.
- Spawned backend factory and reactor execution have separate unwind boundaries. On reactor panic,
  the request registry fails buffered and streaming terminals with the canonical Internal error
  before backend-held streaming sinks can win through Drop.
- Resolver-thread panic is observable at joined shutdown and a disconnected resolver result path
  cannot leave an accepted HTTP request waiting forever.
- Manual-drive backend panic leaves its reentrancy guard usable through a Drop guard. It is allowed
  to unwind to the unique manual owner rather than pretending arbitrary in-process corruption can
  safely resume.
- Shutdown-from-callback, detached callback joining, callback-worker exit, and concurrent admission
  during shutdown already have adversarial lifecycle regressions.

## 4. Diagnostics and TLS policy

- `Error::message()` is the payload-free diagnostic. Stable decisions use `ErrorKind`,
  `TransportStage`, `TimeoutKind`, `LimitKind`, and the new `TlsFailure` category.
- Raw rustls error text is no longer copied into public errors. `TlsFailure` distinguishes
  configuration, hostname mismatch, unknown issuer, expired/not-yet-valid/revoked/other certificate,
  peer alert, protocol, local I/O, and unknown failures without retaining hostnames, certificates,
  alert values, or backend-native messages. Wrong-host, unknown-root, expiry, and peer-alert tests
  lock this down.
- No verbose raw-TLS flag is provided. A debugger can inspect internals in a controlled development
  build; production callers receive a useful structured reason without an accidental logging path.
- TLS chain and hostname verification remains the default. The deliberately named
  `DangerouslyDisableCertificateVerification` compatibility option still verifies handshake
  signatures and is isolated in the pool key so verified and unverified connections cannot mix.
- Redirect policy rejects HTTPS-to-HTTP downgrade and strips credentials on cross-origin hops.

Request and response values are intentionally owned public data and their `Debug` output is not a
redaction boundary. Applications remain responsible for values they explicitly choose to log.

## 5. Resource and shutdown bounds

Default owner-level ceilings remain 16 MiB buffered request/response bodies, 64 KiB and 256 fields
per header set, 256 KiB per streaming window, 16 MiB aggregate queued stream data, 32 global / 8
per-origin active connections, 32 global / 4 per-origin idle connections, and 30 seconds idle
lifetime. Admission, callback events, commands, resolver work, TLS flights, parser metadata, socket
queues, deadlines, and pools are separately bounded. Limits are checked before buffer growth where
untrusted sizes enter the owner.

Cancellation closes local work but does not claim to recall bytes already accepted by the kernel.
Consuming shutdown rejects new admission, cancels accepted requests, joins resolver/reactor work,
and resolves or explicitly detaches the already-network-free callback domain.

## 6. Public API and MSRV

- `RunMode`, `CallbackDispatch`, `TlsVerification`, and `RequestOptions` are now non-exhaustive.
  Consumers already have constructors/builders and `Default`; future policy fields or modes need
  not force a breaking release solely because exhaustive matching or struct literals were allowed.
- Other evolving public classifications were already non-exhaustive. Unique `Engine`, cloneable
  `Client`, streaming producer/reader ownership, and consuming shutdown remain unchanged.
- Rust 1.85 remains the MSRV. New Rust 1.99 nightlies deprecate `Atomic*::fetch_update`, while its
  replacement `try_update` requires Rust 1.95. Private compare/exchange helpers replace those calls
  so both MSRV and new-nightly warning-denied builds remain clean; contention and saturation are
  regression-tested.

## 7. Reporting and remaining release gates

Root `SECURITY.md` defines private GitHub vulnerability reporting, supported pre-1.0 versions,
minimum report contents, and the no-production-secrets rule. GitHub private vulnerability reporting
must be enabled when `madandy24/nbreq` is created; until then this is a packaged policy, not a live
contact mechanism.

The 2026-08-24 `cargo-audit 0.22.2` scan loaded 1,225 RustSec advisories and checked 139 locked
packages. It found `time 0.3.45` through the dev-only certificate fixtures. Fixed `time 0.3.47`
requires Rust 1.88, which the exact-source gate caught after the initial lock update. The test graph
therefore keeps the newest Rust-1.85-compatible release, 0.3.45; 0.3.46 also requires Rust 1.88.
`RUSTSEC-2026-0009` affects only
RFC-2822 parsing; rcgen selects `time`'s `std`/`alloc` features without `parsing`, and NBReq uses it
only to construct generated certificate dates. This is a dev-only reviewed exception, not runtime
exposure. The scan also matched two Hickory advisories that are reviewed exceptions rather than
reachable NBReq paths:

- `RUSTSEC-2026-0118` requires Hickory's DNSSEC feature and `DnssecDnsHandle`; NBReq compiles neither.
- `RUSTSEC-2026-0119` requires encoding malicious messages with many records. The only production
  encoder call is `prepare_name_query`, which emits one wire-bounded A/AAAA question and no records;
  untrusted responses are decoded. A dedicated regression locks this shape.

Hickory 0.26.1 fixes the encoder but requires Rust 1.88. The audited command succeeds with the
dev-only `RUSTSEC-2026-0009` and two unreachable Hickory IDs explicitly ignored, preserving the
stated Rust-1.85 MSRV. Eliminating the wire-only Hickory dependency is promoted to early post-WP11
work, and any expansion of its features or encoder use invalidates this assessment.

Still outside this checkpoint:

- inspect packaged archives after `nbreq-winpoll` exists in the registry;
- add CI and repository security settings on the real public host;
- keep GDS ureq rollback through the initial public observation period.

The complete 20-stage Windows verifier passes in 67.232 seconds after the audit changes. Its first
run exposed a parallel-test laboratory race: a just-released refused-connect port could be acquired
by another adversarial fixture. All loopback fixture allocation is now serialized until the refused
connection is observed; the complete adversarial suite passes ten additional repetitions before the
clean full gate.

The final MSRV-preserving graph is commit `de21963`, archived as 563,206 bytes with SHA-256
`115E37BCF017AEECB4595BEC9A818E8BCB5E791C66CEAC5BAAF90926B8B3448A`. The complete 20-stage
Windows verifier passes in 72.057 seconds. A fresh extraction of that authenticated archive then
passes all 20 stages offline on Ubuntu 20.04.6 LTS with Rust/Cargo 1.85.0 in 265.644 seconds and
records `EXIT=0`. WP11.2 is accepted; live repository controls and publication rehearsal move to
WP11.3.
