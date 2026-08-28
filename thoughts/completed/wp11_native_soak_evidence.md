# WP11 native GDS overnight-soak evidence

Status: accepted observation slice, 2026-08-24. This is live lifecycle evidence for the native
backend, not a performance comparison, multi-platform soak, fleet rollout, or 1.0 stability claim.

## Run boundary

- Host/application: the controlled `DMOUSE3#C` GDS development instance.
- Selection: process-local `/httpbackend nbreq-native`; the startup log records NBReq native and
  the GDS compatibility adapter's explicit TLS-verification bypass at `00:57:34`.
- Both primary and backup WebRPC pollers started at `00:57:35`. A normal settings refresh completed
  at `00:58:05`, after which the recreated owners remained active until process close at `11:12:15`.
- Total process observation was 10 hours, 14 minutes, 41 seconds. The uninterrupted post-refresh
  owner interval was 10 hours, 14 minutes, 10 seconds.

## Observed traffic and shutdown

- Both long-poll channels continued producing activity through the final minute. The website was
  still authenticated and responding normally when the operator ended the run.
- The application log contains 40 `Respond` records and 40 matching `Successfully posted: OK`
  acknowledgements during this process.
- No unexpected HTTP, transport, timeout, or request-failure record appears in the run boundary.
  The two final `HTTP request cancelled` lines are the expected individual long-poll cancellation
  path during owner shutdown.
- Both final poller threads exited and joined. Their `DPWebRPC` Drops report 1 ms and 0 ms, and the
  exact-name process check after close found no GDS process.

The source logs are local operational evidence and are deliberately not copied into the NBReq
repository because they contain application payloads. This record retains only counts, lifecycle
markers, and safe diagnostics relevant to NBReq.

## Disposition

This closes the first realistic multi-hour Windows/GDS native observation item and supports using
`0.1.0` for the initial public release. It does not replace exact-source Windows/Linux verification,
Wine evidence, longer repeated observation after publication, or the production-observation gate
before 1.0.
