//! Connect to an echo server: `cargo run --example tcp_echo -- 127.0.0.1:9000`.
use std::net::SocketAddr;
use std::time::Duration;

use nbreq::{Engine, TcpConnectRequest};

fn exchange(engine: &Engine, address: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let request = TcpConnectRequest::literal(address)
        .connect_timeout(Duration::from_secs(5))
        .read_inactivity_timeout(Duration::from_secs(10))
        .write_inactivity_timeout(Duration::from_secs(10))
        .send_queue_bytes(16 * 1024)
        .receive_queue_bytes(16 * 1024)
        .build()?;
    let mut connection = engine.tcp_connector().execute(request)?;
    let sent = b"hello from nbreq\n";
    connection.send(sent.to_vec())?;
    connection.finish()?; // Drain our output and half-close; the reader remains usable.

    let mut received = Vec::new();
    let mut buffer = [0_u8; 256];
    while let Some(count) = connection.read(&mut buffer)? {
        if received.len() + count > sent.len() {
            return Err("echo server returned more bytes than sent".into());
        }
        received.extend_from_slice(&buffer[..count]);
    }
    if received != sent {
        return Err("echo server returned different bytes".into());
    }
    println!("echoed {} bytes and received EOF", received.len());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address = std::env::args()
        .nth(1)
        .ok_or("usage: tcp_echo IP:PORT")?
        .parse()?;
    let engine = Engine::builder().build()?;
    let result = exchange(&engine, address);
    engine.shutdown()?;
    result
}
