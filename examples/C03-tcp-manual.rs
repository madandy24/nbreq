//! Drive connect and connected I/O: `cargo run --example C03-tcp-manual -- [IP:PORT]`.
#[path = "support/echo.rs"]
mod echo;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use nbreq::{
    Engine, EngineBuilder, TcpConnectCompletion, TcpConnectRequest, TcpFinishStatus, TcpRead,
    TcpSendErrorKind,
};

fn exchange(engine: &mut Engine, address: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let pending = engine.tcp_connector().submit(
        TcpConnectRequest::literal(address)
            .connect_timeout(Duration::from_secs(5))
            .read_inactivity_timeout(Duration::from_secs(10))
            .write_inactivity_timeout(Duration::from_secs(10))
            .send_queue_bytes(1024)
            .receive_queue_bytes(1024)
            .build()?,
    )?;
    let mut connection = match engine.drive_until(pending)? {
        TcpConnectCompletion::Completed(connection) => connection,
        TcpConnectCompletion::Failed(error) => return Err(error.into()),
        TcpConnectCompletion::Cancelled => return Err("connect cancelled".into()),
        _ => return Err("unrecognized connect completion".into()),
    };
    let sent = b"hello from nbreq\n";
    let mut unsent = Some(sent.to_vec());
    let mut received = Vec::new();
    let mut buffer = [0_u8; 256];
    let mut finished = false;
    let mut eof = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !finished || !eof {
        if Instant::now() >= deadline {
            return Err("echo deadline elapsed".into());
        }
        // Manual mode uses try_* calls. Blocking send/read/finish would return WrongMode.
        if let Some(bytes) = unsent.take() {
            match connection.try_send(bytes) {
                Ok(()) => {}
                Err(error) if error.kind() == TcpSendErrorKind::WouldBlock => {
                    unsent = Some(error.into_remaining())
                }
                Err(error) => return Err(error.into()),
            }
        }
        if unsent.is_none() && !finished {
            finished = connection.try_finish()? == TcpFinishStatus::Finished;
        }
        match connection.try_read(&mut buffer)? {
            TcpRead::Data(count) => {
                if received.len() + count > sent.len() {
                    return Err("oversized echo".into());
                }
                received.extend_from_slice(&buffer[..count]);
            }
            TcpRead::Pending => {}
            TcpRead::Eof => eof = true,
            _ => return Err("unrecognized read outcome".into()),
        }
        if !finished || !eof {
            engine.drive((Instant::now() + Duration::from_millis(10)).min(deadline))?;
        }
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
    let mut engine = EngineBuilder::manual().build()?;
    let result = exchange(&mut engine, address);
    engine.shutdown()?;
    if let Some(server) = server {
        server.stop()?;
    }
    result
}
