# Upgrading to NBReq 0.3.0

Version 0.3.0 is currently unreleased. It introduces finite defaults for common
network operations. Existing `"0.2"` Cargo requirements stay on the 0.2 line;
select `nbreq = "0.3.0"` when adopting this release.

## New timeout defaults

Buffered HTTP requests, including the Engine GET/POST convenience calls and
`RequestOptions::default()`, now allow ten seconds for connection establishment,
thirty seconds without useful I/O progress, and 120 seconds overall. Public DNS
resolution has a thirty-second total deadline. TCP connection establishment has
a ten-second deadline, and pending TCP/TLS output has a thirty-second write
inactivity deadline.

**A healthy, quiet TCP/TLS connection stays open.** Connected read inactivity
remains disabled by default; the write timer is inactive when no accepted output
awaits progress. There is no overall TCP/TLS connection lifetime limit.

Fresh `StreamRequest` builders use the HTTP connect and inactivity defaults but
have no total-duration limit. A quiet event stream or slow upload producer may
still need `inactivity_timeout(None)`. Converting an existing `Request` into a
`StreamRequest` preserves all its options, including the buffered request's
120-second total timeout. Likewise, `.options(RequestOptions::default())` replaces
the stream's complete options with buffered defaults. Configure the options you
intend; conversion and submission never infer whether a value was explicit.

These defaults can end slow requests, long polling and stalled transfers that
previously waited indefinitely. Explicitly configured durations still take
precedence. Existing duration-based calls compile unchanged.

## Select a duration or disable a timer

HTTP, DNS and TCP timeout setters now accept `Duration`, `Some(Duration)` or
`None`. Omitting a setting uses its default. Passing `None` explicitly disables
that timer; it never means "inherit the default". The last setter wins, and
zero is not a synonym for disabling a timer.

To restore the previous unbounded HTTP policy:

```rust
use nbreq::Request;

let request = Request::get("https://example.com/")
    .connect_timeout(None)
    .inactivity_timeout(None)
    .total_timeout(None)
    .build()?;
# Ok::<(), nbreq::Error>(())
```

The Engine GET/POST convenience builders and streaming builders accept the same
three setters. Setting the corresponding public `RequestOptions` fields to
`None` has the same effect.

For plain TCP, disable establishment and write inactivity to recover its former
defaults; read inactivity is already disabled:

```rust
use nbreq::TcpConnectRequest;

let request = TcpConnectRequest::hostname("example.com", 9000)
    .connect_timeout(None)
    .write_inactivity_timeout(None)
    .build()?;
# Ok::<(), nbreq::Error>(())
```

For DNS, use `ResolveRequest::hostname(name).total_timeout(None)`. This removes
the operation deadline while retaining the resolver's existing finite internal
retry budgets. Internal HTTP/TCP lookups use their enclosing connect/total
budgets; they do not gain an additional thirty seconds.

## Direct TLS and STARTTLS

`TlsOptions` retains its existing finite ten-second establishment deadline and
its `Duration` setter. Direct TLS uses the earlier of this deadline and the TCP
connect deadline, covering DNS, TCP, handshake and certificate verification.
To allow a longer direct-TLS establishment, raise or disable the TCP connect
deadline as well:

```rust
use std::time::Duration;
use nbreq::{TcpConnectRequest, TlsOptions};

let request = TcpConnectRequest::hostname("mail.example.org", 993)
    .connect_timeout(None)
    .build()?;
let tls = TlsOptions::new("mail.example.org")?
    .handshake_timeout(Duration::from_secs(30));
# Ok::<(), nbreq::Error>(())
```

An upgrade starts a fresh TLS deadline; it does not reuse the completed TCP
connection's establishment budget. Established TLS keeps the selected connected
read/write policies. The new `None` support does not remove the existing finite
TLS handshake requirement.

## Unchanged clock and resource semantics

Operation deadlines start at admission, including time queued inside the Engine.
HTTP redirects retain the original total deadline while renewing the existing
per-hop connect/inactivity clocks. Useful I/O progress resets inactivity, and
response-consumer backpressure pauses it; waiting for an empty streaming upload
producer does not pause HTTP inactivity.

`wait_for` still bounds only a local wait and leaves the underlying operation
live. Manual Engines still require explicit driving to process deadlines.
Timeouts do not impose a hard bound on executing user callbacks or platform
certificate checks during joined shutdown.

Queue sizes, body limits, connection limits, DNS selection/retry policy, TLS
verification and dependency requirements are unchanged. See the
[defaults table](https://github.com/madandy24/nbreq/blob/main/docs/getting-started.md#timeout-and-queue-defaults)
for the complete policy, and the [0.2 migration guide](https://github.com/madandy24/nbreq/blob/v0.2.1/docs/migrating-to-0.2.md)
for earlier API changes.
