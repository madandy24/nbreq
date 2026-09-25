//! A small STARTTLS-style upgrade: `cargo run --example C05-tcp-starttls`.
//! The local text greeting is illustrative; this example is not an SMTP or IMAP client.
#[path = "support/tls_echo.rs"]
mod tls_echo;

use std::time::Duration;

use nbreq::{Engine, EngineConfig, TcpConnectRequest, TcpConnection, TlsOptions};

fn read_line(connection: &mut TcpConnection) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut line = Vec::new();
    while line.len() < 128 {
        // Consume the complete text reply without handing protocol bytes to the TLS parser.
        // The Engine separately rejects an upgrade if it has buffered extra plaintext.
        let mut byte = [0_u8; 1];
        if connection.read(&mut byte)? != Some(1) {
            return Err("plain peer closed before the upgrade response".into());
        }
        line.push(byte[0]);
        if line.ends_with(b"\r\n") {
            return Ok(line);
        }
    }
    Err("plain protocol line exceeded 128 bytes".into())
}

fn exchange(engine: &Engine, server: &tls_echo::Server) -> Result<(), Box<dyn std::error::Error>> {
    let request = TcpConnectRequest::literal(server.address())
        .connect_timeout(Duration::from_secs(5))
        .read_inactivity_timeout(Duration::from_secs(10))
        .write_inactivity_timeout(Duration::from_secs(10))
        .send_queue_bytes(1024)
        .receive_queue_bytes(1024)
        .build()?;
    let mut plain = engine.tcp_connector().execute(request)?;
    if read_line(&mut plain)? != b"220 local.example ready\r\n" {
        return Err("unexpected local greeting".into());
    }
    plain.send(b"STARTTLS\r\n".to_vec())?;
    if read_line(&mut plain)? != b"220 Ready to start TLS\r\n" {
        return Err("local peer did not accept STARTTLS".into());
    }

    // `into_tls` consumes the unsplit plain connection. There is no simultaneous cleartext
    // handle or fallback after the handshake starts.
    let tls = TlsOptions::new("127.0.0.1")?.handshake_timeout(Duration::from_secs(5));
    let mut secure = plain.into_tls(tls)?;
    secure.send(tls_echo::MESSAGE.to_vec())?;
    // The local fixture negotiates TLS 1.3, which permits a reply after our close_notify.
    secure.finish()?;
    let mut received = Vec::new();
    let mut buffer = [0_u8; 256];
    while let Some(count) = secure.read(&mut buffer)? {
        if received.len() + count > tls_echo::MESSAGE.len() {
            return Err("local TLS peer sent too many bytes".into());
        }
        received.extend_from_slice(&buffer[..count]);
    }
    if received != tls_echo::MESSAGE {
        return Err("protected reply did not match the request".into());
    }
    println!(
        "upgraded the same socket; echoed {} protected bytes",
        received.len()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = tls_echo::Server::start(tls_echo::Mode::StartTls)?;
    let engine = Engine::new(
        EngineConfig::spawned().with_additional_tls_root_certificate(server.root_der().to_vec()),
    )?;
    let result = exchange(&engine, &server);
    let shutdown = engine.shutdown();
    let stopped = server.stop();
    result?;
    shutdown?;
    stopped?;
    Ok(())
}
