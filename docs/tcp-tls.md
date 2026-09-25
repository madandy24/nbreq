# TLS for TCP connections

This guide describes the TCP TLS API under development after NBReq 0.2.0. It is not
available in the published 0.2.0 crate. Implementation and verification status are
tracked in the repository's [TCP TLS plan](../thoughts/nbreq_tcp_tls_plan.md).

Use immediate TLS when the server expects a TLS handshake as soon as TCP connects,
such as an IMAP server on port 993. Use an explicit upgrade when the application
protocol starts in cleartext and negotiates TLS, such as SMTP STARTTLS on port 25.
NBReq supplies the transport; the application supplies the mail protocol.

## Immediate TLS

Create an ordinary TCP request and pass a separate TLS server identity. The identity
is the DNS name or IP address the server's certificate must authenticate. It need
not be the literal address used to reach the server.

```rust,no_run
use std::time::Duration;
use nbreq::{Engine, TcpConnectRequest, TlsOptions};

let engine = Engine::builder().build()?;
let request = TcpConnectRequest::hostname("mail.example.org", 993)
    .connect_timeout(Duration::from_secs(10))
    .read_inactivity_timeout(Duration::from_secs(15))
    .write_inactivity_timeout(Duration::from_secs(15))
    .send_queue_bytes(16 * 1024)
    .receive_queue_bytes(16 * 1024)
    .build()?;
let tls = TlsOptions::new("mail.example.org")?
    .handshake_timeout(Duration::from_secs(10));
let mut connection = engine.tcp_connector().execute_tls(request, tls)?;

// Certificate verification and the handshake have completed before this returns.
let mut greeting = [0_u8; 1024];
let count = connection.read(&mut greeting)?;
println!("Received greeting bytes: {count:?}");
drop(connection);
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`mail.example.org` is a placeholder for your server. A read may return only part of
a protocol line; production protocol code must accumulate and parse a bounded
response. TCP and TLS do not supply message boundaries.

Verification uses platform trust and any additional CA roots configured on the
Engine. Certificate signatures, validity and server identity remain checked. IP
identities require a matching IP certificate identity and do not send DNS SNI.
This API has no verification bypass or automatic plaintext fallback. Standalone
TLS offers no ALPN protocols; HTTPS keeps its existing HTTP policy.

## Upgrading an existing connection

Keep the plain `TcpConnection` unsplit while negotiating the upgrade. Read and
validate the protocol's complete positive upgrade response, then consume that
connection with `into_tls(options)` for a blocking upgrade, or
`submit_tls(options)` / `start_tls(options, callback)` for nonblocking completion.

For SMTP this means reading the greeting, negotiating capabilities with EHLO,
checking STARTTLS support, issuing STARTTLS, and accepting the server's positive
response before starting TLS. After TLS, issue EHLO again and rebuild capabilities
from the authenticated session. Never infer successful TLS from a server's
cleartext advertisement alone.

The upgrade has a strict byte boundary:

- There must be no unread data in NBReq's plain receive queue, queued plain output,
  pending output drain, finish request, EOF, or existing failure.
- Your own protocol buffer must also be empty at the boundary. Bytes read before
  TLS are not authenticated application input and must not be carried into the
  protected session.
- Upgrade consumes the connection. **Every upgrade error closes it**, including
  an admission or dirty-boundary rejection. Reconnect if the protocol permits a
  retry; do not continue using plaintext.

NBReq freezes plain I/O when it accepts the transition and retains the same socket,
connection slot and cancellation identity. Upgrade is unavailable on split halves.
The result is a distinct `TlsConnection` with no raw-socket or plaintext escape.

## Waiting, cancellation and manual engines

The connector's `start_tls`, `submit_tls` and `execute_tls` mirror its ordinary TCP
entry points. Both immediate connection and upgrade produce
`TlsConnectCompletion::{Completed, Failed, Cancelled}`. A completed connection has
the familiar read, send, split, finish and cancellation operations.

Use `submit_tls` with a manual Engine, then pass the pending operation to
`engine.drive_until(pending)` or explicitly drive and poll it. Connected manual
I/O uses `try_*` operations with continued driving. `execute_tls`, `into_tls` and
connected blocking read/send/finish helpers reject manual mode. Direct waiters are
passive: `wait()` cannot make progress on an undriven Engine. None of these
operations drives the Engine implicitly.

The handshake timeout defaults to ten seconds, starts at admission, and includes
queueing for certificate verification. For immediate TLS it also covers DNS and
TCP establishment, with an earlier TCP connect deadline taking precedence. A
waiter-local timeout returns the still-live waiter; it does not cancel the
operation. Zero handshake timeout is rejected at admission.

Cancellation is abortive. Engine shutdown closes sockets and joins its owned
workers. An executing platform certificate check may delay joining; the timeout
does not make that platform operation interruptible.

## Closing and errors

`finish` drains accepted application output and sends TLS `close_notify` before
closing the write side. In TLS 1.3 the local reader remains usable, so the peer can
continue sending application data.
TLS 1.2 peer closure ends both application directions; unsent accepted output
causes an explicit failure instead of a successful finish.

Orderly closure delivers queued authenticated input before EOF. A bare TCP EOF is
a TLS truncation failure, not a successful TLS close. Abrupt TLS or transport
failure discards unread NBReq queues, following ordinary TCP abort semantics.
Bytes already returned to the application remain its responsibility. Dropping an
unfinished connection is abortive, so use the protocol's logout/quit exchange and
an orderly TLS close when the protocol requires them.

## Resource use

TLS connections share the Engine reactor and bounded certificate-verification
service. They add no per-connection thread. Each connection reserves its selected
plain send/receive windows plus a 256 KiB TLS staging allowance against the shared
queued-byte limit. These are admission reservations, not eager allocations.

Small windows remain supported: retained decrypted records are delivered in pieces
as the application provides capacity. Accepted plaintext output keeps its queue
credit until its encrypted output drains. Control records do not refund that
credit. Slow readers and writers therefore exert backpressure.

These bounds cover NBReq's specified buffers. They are not a process-memory limit:
certificate verification, rustls internals, platform libraries, allocator overhead,
thread stacks, kernel sockets and application buffers need separate headroom.
Use workload admission and smaller per-connection windows as well as byte budgets.
