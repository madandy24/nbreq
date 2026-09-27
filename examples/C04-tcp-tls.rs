//! Verified immediate TLS: `cargo run --example C04-tcp-tls`.
//! Starts a local peer with a generated private CA; no external service is needed.
#[path = "support/tls_echo.rs"]
mod tls_echo;

use std::time::Duration;

use nbreq::{Engine, EngineConfig, TcpConnectRequest, TlsOptions, TlsTrust};

fn exchange(engine: &Engine, server: &tls_echo::Server) -> Result<(), Box<dyn std::error::Error>> {
    let request = TcpConnectRequest::literal(server.address())
        .connect_timeout(Duration::from_secs(5))
        .read_inactivity_timeout(Duration::from_secs(10))
        .write_inactivity_timeout(Duration::from_secs(10))
        .send_queue_bytes(1024)
        .receive_queue_bytes(1024)
        .build()?;
    // The socket endpoint is a literal address. The TLS identity is selected explicitly and
    // checked against the generated certificate using this Engine's private CA root.
    let tls = TlsOptions::new("127.0.0.1")?.handshake_timeout(Duration::from_secs(5));
    // Connect TCP and complete the verified TLS handshake before sending application bytes.
    let mut connection = engine.tcp_connector().execute_tls(request, tls)?;
    connection.send(tls_echo::MESSAGE.to_vec())?;
    connection.finish()?; // This local TLS 1.3 peer permits reading after our close_notify.

    let mut received = Vec::new();
    let mut buffer = [0_u8; 256];
    while let Some(count) = connection.read(&mut buffer)? {
        if received.len() + count > tls_echo::MESSAGE.len() {
            return Err("local TLS peer sent too many bytes".into());
        }
        received.extend_from_slice(&buffer[..count]);
    }
    if received != tls_echo::MESSAGE {
        return Err("protected reply did not match the request".into());
    }
    println!(
        "verified TLS to 127.0.0.1; echoed {} protected bytes",
        received.len()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = tls_echo::Server::start(tls_echo::Mode::Immediate)?;
    // Trust only this local fixture's private CA. For a server trusted by the OS, use
    // EngineConfig::spawned() with its default platform trust and the server's TLS identity.
    let engine = Engine::new(
        EngineConfig::spawned()
            .with_tls_trust(TlsTrust::SuppliedRootsOnly)
            .with_additional_tls_root_certificate(server.root_der().to_vec()),
    )?;
    let result = exchange(&engine, &server);
    let shutdown = engine.shutdown();
    let stopped = server.stop();
    result?;
    shutdown?;
    stopped?;
    Ok(())
}
