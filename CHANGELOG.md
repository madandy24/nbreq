# Changelog

## 0.2.1 — 2026-09-27

- Add verified standalone TLS 1.2/1.3 connections and consuming STARTTLS-style upgrades.
  `TlsOptions`, `TlsConnection`, split reader/writer types, cancellation handles, callbacks
  and direct waiters share the Engine lifecycle. `PendingTlsConnect` supports manual
  `drive_until`; add `Engine::run_mode()` and `TlsFailure::Truncated`.

- Preserve authenticated standalone TLS replies and orderly EOF when the final
  write shutdown reports an already disconnected socket after the peer close alert
  and complete local output drain. Other transport errors remain failures.
- Add explicit Engine TLS trust selection for HTTPS, direct TLS, and STARTTLS:
  platform trust (unchanged default), supplied DER roots only, or Mozilla roots
  plus supplied roots. Both portable modes use WebPKI verification.
- Add the optional `bundled-roots` feature, which enables native support and the
  compact `webpki-roots` bundle without changing the default trust selection.
  Compatible dependency updates can refresh roots without a matching NBReq release;
  applications must update their lockfile, rebuild, and redeploy.
- Reject empty supplied-only trust, malformed roots, and unsupported trust
  configurations during Engine construction. Portable modes do not use OS trust,
  enterprise distrust, or platform revocation retrieval; no silent fallback is added.
- Parse additional DER roots as WebPKI trust anchors before using the selected verifier,
  including platform mode. OS acceptance differences can now cause construction to fail.
  Public `test-support` held/HTTP-only constructors also return `Unsupported` for trust
  settings they cannot apply instead of ignoring them.
- Compact excess capacity in accepted plain TCP send buffers; refused buffers retain their
  original allocation. Use bounded storage for small native receive windows, and keep peer
  FIN terminal so queue-pressure changes cannot restart read inactivity after EOF.
- Add local direct-TLS and TCP-to-TLS upgrade examples to the release example checks.
- Document the Rust 1.85 application-lock workaround for upstream `yoke-derive 0.8.3`;
  published dependency ranges remain unchanged. Newer compilers can use normal resolution.

The existing HTTP/DNS/plain TCP API and default features remain available, with Rust 1.85
as the minimum supported version. The separate `nbreq-smtp` 0.1.0 workspace crate remains
unpublished; this core release does not announce an SMTP package release.
