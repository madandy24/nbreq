# Changelog

## Unreleased

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
