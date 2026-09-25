# NBReq SMTP sender

`nbreq-smtp` sends one prepared message over NBReq's verified TCP/TLS transport. It is a separate, unpublished workspace crate while it depends on the unreleased local TCP/TLS API. It supports required STARTTLS and implicit TLS. It has no plaintext fallback, automatic retry, authentication, SIZE, 8BITMIME, SMTPUTF8, pipelining, MIME builder, mail reader, or server.

The caller supplies the relay and its verified certificate identity, an EHLO name, an envelope, and prepared RFC 5322 message bytes. The message must contain a valid header section and a blank line before the body. Input uses printable seven-bit ASCII plus horizontal tab and canonical CRLF line endings, with at most 998 content bytes per line. A missing final CRLF is appended. Both the canonical length and retained `Vec` capacity must fit 8 MiB; there may be 1–100 recipients. Mailbox and EHLO validation happens before network admission. Envelope local parts support ASCII dot-atom syntax with DNS domains; quoted local parts and mailbox address literals are unsupported. There is no generated `From`, `Date`, or `Message-ID` header, so include the fields your relay and recipients require. The constructor checks framing and basic header syntax; it does not validate full RFC 5322 semantics, `From`/`Date` correctness, or MIME content.

```rust,no_run
use std::time::Duration;
use nbreq::{Engine, EngineConfig, TcpConnectRequest, TlsOptions};
use nbreq_smtp::{Delivery, Envelope, SendRequest, SmtpClient, SmtpServer, TlsPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new(EngineConfig::spawned())?;
    let connect = TcpConnectRequest::hostname("smtp.example.org", 25)
        .connect_timeout(Duration::from_secs(10))
        .send_queue_bytes(16 * 1024)
        .receive_queue_bytes(16 * 1024)
        .build()?;
    let server = SmtpServer::new(
        connect,
        TlsOptions::new("smtp.example.org")?,
        TlsPolicy::RequiredStartTls,
    );
    let envelope = Envelope::new("sender@example.org", vec!["recipient@example.org".into()])?;
    let message = b"From: sender@example.org\r\nTo: recipient@example.org\r\nSubject: Hello\r\n\r\nHello.\r\n".to_vec();
    let request = SendRequest::new(server, "client.example.org", envelope, message)?;
    let outcome = SmtpClient::new(&engine).send_blocking(request)?;
    match &outcome.delivery {
        Delivery::Accepted { reply } => println!("server accepted: {}", reply.code),
        other => println!("submission ended: {other:?}"),
    }
    engine.shutdown()?;
    Ok(())
}
```

`Accepted` means the server returned a positive final DATA completion and accepted responsibility for the transaction. It does not prove delivery to a mailbox. `Rejected` is an explicit server rejection. `NotAccepted` means the complete DATA terminator was not admitted. `Uncertain` means the terminator was admitted but the client did not receive a valid final answer; **do not retry an uncertain submission automatically**. Each requested recipient has an accepted-for-transaction, rejected, unresolved, or unattempted status. The default `RecipientPolicy::RequireAllRecipients` attempts every RCPT and skips DATA if any are rejected. Opt in to `RecipientPolicy::AcceptedRecipients` only when delivery to the accepted subset is desired. A final acceptance remains accepted if QUIT or TLS cleanup later fails.

For caller-driven progress, call `SmtpClient::submit` and repeatedly call `SendOperation::poll`. It returns `std::task::Poll<&SendOutcome>` but is not a `Future`, does not register a waker, and never runs a per-send protocol thread. A manual NBReq Engine also needs `Engine::drive` between polls:

```rust,no_run
use std::time::{Duration, Instant};
use std::task::Poll;
use nbreq::{Engine, EngineConfig, TcpConnectRequest, TlsOptions};
use nbreq_smtp::{Envelope, SendRequest, SmtpClient, SmtpServer, TlsPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = Engine::new(EngineConfig::manual())?;
    let connect = TcpConnectRequest::hostname("smtp.example.org", 25).build()?;
    let server = SmtpServer::new(connect, TlsOptions::new("smtp.example.org")?, TlsPolicy::RequiredStartTls);
    let envelope = Envelope::new("sender@example.org", vec!["recipient@example.org".into()])?;
    let message = b"From: sender@example.org\r\nSubject: Hello\r\n\r\nHello.\r\n".to_vec();
    let request = SendRequest::new(server, "client.example.org", envelope, message)?;
    let mut operation = SmtpClient::new(&engine).submit(request)?;
    loop {
        if let Poll::Ready(outcome) = operation.poll() {
            println!("{:?}", outcome.delivery);
            break;
        }
        engine.drive(Instant::now() + Duration::from_millis(10))?;
    }
    engine.shutdown()?;
    Ok(())
}
```

SMTP phase and overall deadlines are checked when `poll` runs. Leaving an operation unpolled does not schedule an SMTP watchdog or protocol progress; underlying NBReq transport deadlines still run while its Engine progresses. Cancellation aborts a pending operation and reports the appropriate `NotAccepted` or `Uncertain` state. Dropping an operation aborts its pending connect or live connection. `send_blocking` is available only with a spawned Engine.

The command-line [send_message example](examples/send_message.rs) requires every destination and envelope argument explicitly:

```text
cargo run -p nbreq-smtp --example send_message -- HOST PORT starttls|implicit EHLO_NAME FROM TO MESSAGE_FILE
```

The file must end in CRLF so its prepared bytes are preserved before SMTP dot-stuffing. Exit status 0 means server acceptance, 2 explicit rejection, 3 not accepted, 4 uncertain, and 1 input or startup failure. The example prints the final server reply with escaped text, including a queue ID if one was returned. It never retries.

Replies are bounded to 512 bytes per line, 32 lines, and 8 KiB total. Retained recipient reply summaries can add roughly 800 KiB plus per-recipient metadata at the 100-recipient cap. Protocol output staging is at most 4 KiB, with at most 64 state/I/O steps and 16 KiB DATA admission per poll. Defaults are a 30-second command deadline and a 120-second overall deadline; both are configurable and must be finite. TCP queue windows may be as small as one byte. These limits bound the crate's own queues and parsing, not the process's total memory.

TLS identity verification uses NBReq's platform trust and any additional roots configured on the Engine. It is independent of a literal socket address. On STARTTLS, the client discards all pre-upgrade capabilities and sends EHLO again after verified TLS. No mail transaction command is sent if the required upgrade fails. The reviewed NBReq transport has an inherited Wine 5 platform-verifier limitation; this crate does not claim verified TLS support on that Wine version.

This workspace crate requires the local NBReq TCP/TLS source. The published NBReq 0.2.0 package does not yet provide that API, so a separate consumer must use this checkout until the transport and SMTP packages are released together.
