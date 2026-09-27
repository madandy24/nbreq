# TLS for TCP connections

This guide describes the TCP TLS API in NBReq 0.2.1. Start with
[C04: direct TCP+TLS](../examples/C04-tcp-tls.rs) or
[C05: upgrade TCP to TLS](../examples/C05-tcp-starttls.rs). Both use local verified
TLS peers; see the [example commands and expected output](../examples/README.md#c--tcp).

Use immediate TLS when the server expects a TLS handshake as soon as TCP connects,
such as an IMAP server on port 993. Use an explicit upgrade when the application
protocol starts in cleartext and negotiates TLS, such as SMTP STARTTLS on port 25.
NBReq supplies the transport; the application supplies the mail protocol.

The default features include both `native` and `resolver`. TLS requires `native`,
which includes internal exact-name DNS for `TcpConnectRequest::hostname` even
without `resolver`. The `resolver` feature adds the public Resolver API and search
expansion. A literal socket address can also be paired with a separate certificate
DNS name or IP identity through `TlsOptions`.

## Immediate TLS

Create an ordinary TCP request and pass a separate TLS server identity. The identity
is the DNS name or IP address the server's certificate must authenticate. It need
not be the literal address used to reach the server.

```rust,no_run
use std::time::Duration;
use nbreq::{Engine, TcpConnectRequest, TlsOptions};

let engine = Engine::builder().build()?;
let request = TcpConnectRequest::hostname("mail.example.org", 993)
    .read_inactivity_timeout(Duration::from_secs(15))
    .build()?;
let tls = TlsOptions::new("mail.example.org")?;
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

This example uses the default ten-second TLS establishment deadline and Engine queue windows.
It keeps a read inactivity timeout because waiting for the application greeting has no default
deadline. See [timeout and queue defaults](getting-started.md#timeout-and-queue-defaults) for
the defaults and override methods; [C04](../examples/C04-tcp-tls.rs) and
[C05](../examples/C05-tcp-starttls.rs) show explicit timeout and queue settings.

Verification uses the Engine's selected trust policy, defaulting to platform trust
and any additional CA roots. Certificate signatures, validity and server identity remain checked. IP
identities require a matching IP certificate identity and do not send DNS SNI.
This API has no verification bypass or automatic plaintext fallback. Standalone
TLS offers no ALPN protocols; HTTPS keeps its existing HTTP policy.

Win32 portable-trust tests and verified live IMAPS/SMTP STARTTLS transport probes
passed on Wine 5.0 (Ubuntu package 5.0-3ubuntu1), using a private Win32 prefix and an
app-local ProcessPrng shim. The live probes selected `BundledMozilla` and performed
only unauthenticated greeting/capability/quit exchanges. Platform certificate setup
or validation still failed there. This does not establish compatibility for all
Wine versions or SMTP message delivery; validate the intended
deployment and explicitly selected trust policy.

## Selecting certificate trust

NBReq 0.2.1 applies one immutable trust policy to HTTPS (including redirects),
direct TLS and STARTTLS. Choose it explicitly when constructing the Engine:

| `TlsTrust` mode | Roots used | Required feature |
| --- | --- | --- |
| `Platform` (default) | OS trust plus additional DER roots | `native` |
| `SuppliedRootsOnly` | Only additional DER roots; at least one required | `native` |
| `BundledMozilla` | Compiled Mozilla anchors plus additional DER roots | `bundled-roots` |

```rust,no_run
use nbreq::{Engine, EngineConfig, TlsTrust};

let config = EngineConfig::spawned()
    .with_tls_trust(TlsTrust::SuppliedRootsOnly)
    .with_additional_tls_root_certificate(std::fs::read("company-root.der")?);
let engine = Engine::new(config)?;
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

For public Mozilla roots, enable the feature and explicitly select the policy:

```toml
[dependencies]
nbreq = { version = "0.2.1", features = ["bundled-roots"] }
```

```rust,no_run
use nbreq::{Engine, EngineConfig, TlsTrust};

let engine = Engine::new(
    EngineConfig::spawned().with_tls_trust(TlsTrust::BundledMozilla),
)?;
engine.shutdown()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The feature implies `native` but does not change the default policy. Custom DER roots
may be added before or after selecting a mode. They are parsed as WebPKI trust anchors
before entering the selected verifier, including platform mode. Certificates accepted
or ignored differently by the OS in 0.2.0 can therefore fail construction in 0.2.1.
Malformed roots, an empty supplied-only set, and unsupported selections fail during
Engine construction, before network work; no platform fallback occurs. Backends that
cannot apply configured trust return `Unsupported`, including public `test-support`
held/HTTP-only constructors.

Both portable modes use WebPKI for certificate signatures, validity and hostname checks.
They do not inherit OS enterprise roots, distrust rules or revocation retrieval, and do
not add online revocation checking. Platform mode retains its platform policy. Use separate
Engines for separate trust domains. The Android restriction on extra platform roots does
not apply to portable roots.

The Mozilla bundle comes from compact `webpki-roots` trust anchors, not full certificate
parsing on each connection. Its compatible dependency range lets an application update
`webpki-roots` without waiting for a matching NBReq release. Update the application's lockfile,
rebuild and redeploy to use new roots; running binaries do not refresh automatically.

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
operation. Zero or unrepresentable handshake timeouts are rejected at admission.

Cancellation is abortive. Engine shutdown closes sockets and joins its owned
workers. An executing platform certificate check may delay joining; the timeout
does not make that platform operation interruptible.

## Closing and errors

`finish` drains accepted application output and sends TLS `close_notify` before
closing the write side. In TLS 1.3 the local reader remains usable, so the peer can
continue sending application data.
TLS 1.2 peer closure ends both application directions. With no pending application
output, NBReq replies with `close_notify`. With pending output, it aborts the
transport and reports a send failure: skipping already-encrypted records to send
a later close alert would break TLS framing or record sequencing. Bytes already
accepted by the operating system cannot be recalled. This is never reported as a
successful finish.

Orderly closure delivers queued authenticated input before EOF. On an established TLS
connection, a bare TCP EOF is a `TlsFailure::Truncated` failure, not a successful TLS close.
After an authenticated
peer close and complete local output drain, a final write shutdown reporting an
already disconnected socket preserves the reply and orderly EOF. Other transport
errors remain failures. Abrupt TLS or transport
failure discards unread NBReq queues, following ordinary TCP abort semantics.
Bytes already returned to the application remain its responsibility. Dropping an
unfinished connection is abortive, so use the protocol's logout/quit exchange and
an orderly TLS close when the protocol requires them.

## Resource use

TLS connections share the Engine reactor and bounded certificate-verification
service. They add no per-connection thread. Each connection reserves its selected
plain send/receive windows plus a 256 KiB TLS staging allowance against the shared
queued-byte limit. These are admission reservations, not eager allocations.
The current implementation keeps one 64 KiB encrypted staging buffer per live TLS
transport to bound copying and avoid buffer-merging allocation peaks; other
staging is acquired as work requires it.

For example, 16 KiB send and receive windows reserve 288 KiB per TLS connection;
32 such connections reserve 9 MiB. The default 256 KiB windows reserve 768 KiB per
TLS connection, so the default 16 MiB shared budget admits at most 21 of them when
no other work uses that budget. Configure the windows and shared budget together.

Small windows remain supported: retained decrypted records are delivered in pieces
as the application provides capacity. Accepted plaintext output keeps its queue
credit until its encrypted output drains. Control records do not refund that
credit. Slow readers and writers therefore exert backpressure.

These bounds cover NBReq's specified buffers. They are not a process-memory limit:
certificate verification, rustls internals, platform libraries, allocator overhead,
thread stacks, kernel sockets and application buffers need separate headroom.
Use workload admission and smaller per-connection windows as well as byte budgets.
