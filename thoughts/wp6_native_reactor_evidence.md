# WP6 Rust-native reactor evidence

Status: **WP6 accepted on Windows and Ubuntu 20.04.** This slice is deliberately below DNS, TLS,
and HTTP.

## Boundary and ownership

The `native` feature now enables a private `mio`-based readiness core. `mio` is a poll/notification
library, not an async executor or runtime. Version 1.0.4 is pinned, requires Rust 1.70, is MIT
licensed, and adds no dynamically deployed runtime library. NBReq remains `unsafe_code = "forbid"`;
platform-specific unsafe code stays inside the audited dependency.

The core owns:

- every nonblocking TCP socket and readiness registration;
- a cloneable thread-safe wake handle, while the `Poll` owner remains unique;
- generation-checked slot IDs and monotonically allocated poll tokens, so an event from an old
  registration cannot target a reused slot, including on 32-bit platforms;
- bounded outbound queues and cumulative receive limits checked before growth;
- a stale-safe minimum-deadline heap and wait calculation from the caller/nearest deadline;
- connect completion via `SO_ERROR`, read/write draining to `WouldBlock`, peer FIN as read-half
  closure without destroying the local write half, cancellation, deregistration, and idempotent
  shutdown;
- direct external wake plus a 50 ms safety poll whose failure path remains fatal and observable.

No callback, public request type, DNS result, TLS object, or HTTP parser is stored in this layer.
The test-only raw adapter exists solely to prove that the existing `Backend`, `ReactorCore`,
canonical completion, and Engine shutdown paths compose with native readiness before WP7.

## DPGPI extraction audit

Useful mechanisms retained in cleaner form:

- nonblocking connect followed by `SO_ERROR` inspection;
- one polling owner for socket activity;
- an explicit poll notification paired with the command path;
- prompt shutdown notification followed by worker join.

DPGPI structures intentionally not copied:

- its `Arc`/mutex graph, socket clones, and callbacks around mutable slot state;
- rebuilding an fd-to-slot map and registrations on each pass;
- raw file descriptors as event identities without a slot generation;
- fixed 100 ms polling instead of the nearest real deadline;
- `unsafe` registration in NBReq code;
- ignored receive-queue admission errors and callback-oriented queue ownership;
- GDS serial, persistence, TLS, logging, and Delphi component policy.

DPGPI remains valuable prior art, but NBReq's unique Engine and already-proven lifecycle kernel are
the controlling architecture. Improvements discovered here can be considered for DPGPI later
without coupling the projects now.

## Windows proof

The accepted `b367247` source passed 51 unit tests, 4 public-contract tests, and 2 compile-fail
doctests. Native-specific fixtures prove:

- fragmented raw echo and peer half-close;
- outbound and receive bounds before growth;
- an abortive peer close reported and released;
- deadline expiry and slot cleanup;
- cancellation, slot reuse, and stale-generation rejection;
- an external wake interrupting a 30-second poll in under 500 ms;
- spawned Engine raw completion, canonical cancellation, direct native command wakeup, and joined
  shutdown, each under the provisional 500 ms test bound;
- an active manual native Engine moved to another owner thread between drive calls;
- 32 concurrent loopback connections progressing through a four-event readiness buffer;
- 100 repeated reactor create/idempotent-shutdown/drop cycles;
- `NativeReactor` and its wake handle satisfy `Send`.

The ten native-specific fixtures in `b367247` also passed 25 consecutive suite iterations on the recorded
Windows host after the full matrix, with no hang or intermittent failure.

That source's full `--all-features` run was green alongside curl: 68 unit tests, 5 adversarial HTTP
tests, 4 public-contract tests, and 2 doctests. Native-only and all-feature clippy runs pass with
warnings denied; formatting is clean.

## Post-acceptance review hardening

Review before WP7 found that the test-only spawned native adapter advertised a one-hour idle wait.
It now advertises a 50 ms maximum safety poll, matching the curl seam. A deterministic fixture
replaces the external waker with a deliberate failure while the worker is blocked and proves that
both accepted waiters fail, and shutdown observes the failure, within the 500 ms gate.

Peer FIN now marks only the remote read half closed. An idle half-closed socket is deregistered so
it cannot spin, then registered again if the protocol owner queues a final local write. A loopback
fixture proves the peer can half-close, NBReq can observe its final bytes and FIN, and the local
write half can still deliver data before explicit slot cancellation. The current Windows native
suite passes 54 unit tests, 4 public-contract tests, and 2 doctests; strict native clippy and
formatting pass.

## Ubuntu 20.04 proof

The exact `b367247` source archive, SHA-256
`356B588EE03A52EEE5898E12CFBC5CAC15B7C7C467E9709F9E0C14E445D3FF33`, was copied to
`gds-srv-test2`, Ubuntu 20.04.6, and run with Rust/Cargo 1.85.0. The native suite passes 51 unit
tests, 4 public-contract tests, and 2 doctests in 0.49 seconds after its initial dependency build.
Strict native clippy with warnings denied and formatting both pass. The ten native fixtures then
passed 25 consecutive iterations in seven seconds, after which no NBReq test executable remained.

## Acceptance and later boundaries

WP6 is accepted. Windows and Ubuntu prove the same readiness, wake, raw-transfer, cancellation,
manual movement, and teardown contract. Deterministic firewall/refused-connect classification
belongs with the later DNS/connect stage laboratory: this core already proves cancellation of a
connecting slot and deadline-driven release without relying on machine firewall policy.

The feature remains a private foundation. Ordinary `Engine::new` must not silently select it until
the HTTP backend exists and the later parity gates pass.

Poll tokens are never recycled: exhaustion fails closed rather than risking stale-event aliasing.
That is correct for this foundation, but token allocation must be revisited under long-lived
connection-pool load before the native backend is declared release-ready. No Wine native claim is
made, and the Ubuntu acceptance run was native-only; curl's Ubuntu matrix remains separate WP4
evidence.

WP7 may now add HTTP/1.1 serialization and framing with `httparse`; it must not move socket
ownership or cancellation out of this accepted core.
