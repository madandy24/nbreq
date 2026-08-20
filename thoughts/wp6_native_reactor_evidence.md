# WP6 Rust-native reactor evidence

Status: **Windows foundation slice passed; WP6 remains open for Ubuntu proof and review.** This
slice is deliberately below DNS, TLS, and HTTP.

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
- connect completion via `SO_ERROR`, read/write draining to `WouldBlock`, EOF/half-close,
  cancellation, deregistration, and idempotent shutdown.

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

`cargo test --features native` passes 51 unit tests, 4 public-contract tests, and 2 compile-fail
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

The ten native-specific fixtures also passed 25 consecutive suite iterations on the recorded
Windows host after the full matrix, with no hang or intermittent failure.

The full `--all-features` run remains green alongside curl: 68 unit tests, 5 adversarial HTTP
tests, 4 public-contract tests, and 2 doctests. Native-only and all-feature clippy runs pass with
warnings denied; formatting is clean.

## Remaining WP6 gates

- Run the native-only suite on the Ubuntu 20.04 / Rust 1.85 target and record poll/wake behaviour.
- Review resource cleanup under a longer repeated/high-concurrency run and, where available, an
  OS handle/socket baseline.
- Decide whether deterministic refused-connect/firewall fixtures belong at the end of WP6 or with
  the DNS/connect stage laboratory; finite native deadlines already close and release the slot.
- Retain the feature as a private foundation: ordinary `Engine::new` must not silently select it
  until the HTTP backend exists and the later parity gates pass.

WP7 may begin only after this foundation is reviewed and the remaining WP6 platform evidence is
accepted. WP7 will add HTTP/1.1 serialization and framing with `httparse`; it must not move socket
ownership or cancellation out of this core.
