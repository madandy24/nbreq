//! Blocking echo exchange: `cargo run --example C01-tcp-blocking -- [IP:PORT]`.
//! With no address, starts a local echo server; no other setup is needed.
#[path = "support/echo.rs"]
mod echo;

use std::net::SocketAddr;
use std::time::Duration;

use nbreq::{Engine, TcpConnectRequest};

fn exchange(engine: &Engine, address: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let request = TcpConnectRequest::literal(address)
        .connect_timeout(Duration::from_secs(5))
        .read_inactivity_timeout(Duration::from_secs(10))
        .write_inactivity_timeout(Duration::from_secs(10))
        .send_queue_bytes(1024)
        .receive_queue_bytes(1024)
        .build()?;
    let mut connection = engine.tcp_connector().execute(request)?;
    let sent = b"hello from nbreq\n";
    connection.send(sent.to_vec())?;
    connection.finish()?; // Drain output and half-close; the reader remains usable.

    let mut received = Vec::new();
    let mut buffer = [0_u8; 256];
    while let Some(count) = connection.read(&mut buffer)? {
        if received.len() + count > sent.len() {
            return Err("echo server returned more bytes than sent".into());
        }
        received.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(received, sent, "echo must match the sent bytes");
    println!("echoed {} bytes and received EOF", received.len());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = std::env::args().nth(1);
    let server = if destination.is_none() {
        Some(echo::Server::start()?)
    } else {
        None
    };
    let address = match destination {
        Some(address) => address.parse()?,
        None => server
            .as_ref()
            .ok_or("missing local echo server")?
            .address(),
    };
    let engine = Engine::builder().build()?;
    let result = exchange(&engine, address);
    engine.shutdown()?;
    if let Some(server) = server {
        server.stop()?;
    }
    result
}
