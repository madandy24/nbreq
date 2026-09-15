//! Split, nonblocking TCP: `cargo run --example C02-tcp-nonblocking -- [HOSTNAME PORT]`.
//! No arguments uses the supplied loopback server; a hostname uses NBReq's own DNS resolver.
#[path = "support/echo.rs"]
mod echo;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use nbreq::{
    Engine, TcpConnectCompletion, TcpConnectRequest, TcpFinishStatus, TcpRead, TcpSendErrorKind,
};

fn exchange(engine: &Engine, request: TcpConnectRequest) -> Result<(), Box<dyn std::error::Error>> {
    let pending = engine.tcp_connector().submit(request)?;
    println!("connect submitted; application can do other work");
    let connection = match pending.wait() {
        TcpConnectCompletion::Completed(connection) => connection,
        TcpConnectCompletion::Failed(error) => return Err(error.into()),
        TcpConnectCompletion::Cancelled => return Err("connect cancelled".into()),
        _ => return Err("unrecognized connect completion".into()),
    };
    let (mut reader, mut writer) = connection.split();
    let sent = b"hello from nbreq\n";
    // Each chunk must fit the configured send window. Refused chunks remain ours to retry.
    let mut chunks: VecDeque<_> = sent.chunks(8).map(<[u8]>::to_vec).collect();
    let mut received = Vec::new();
    let mut buffer = [0_u8; 256];
    let mut finished = false;
    let mut eof = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !finished || !eof {
        if Instant::now() >= deadline {
            return Err("echo deadline elapsed".into());
        }
        if let Some(chunk) = chunks.pop_front() {
            match writer.try_send(chunk) {
                Ok(()) => {}
                Err(error) if error.kind() == TcpSendErrorKind::WouldBlock => {
                    chunks.push_front(error.into_remaining())
                }
                Err(error) => return Err(error.into()),
            }
        }
        if chunks.is_empty() && !finished {
            finished = writer.try_finish()? == TcpFinishStatus::Finished;
        }
        match reader.try_read(&mut buffer)? {
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
        // A small polling example: put application work here and avoid a busy loop.
        if !finished || !eof {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(received, sent, "echo must match the sent bytes");
    println!("echoed {} bytes and received EOF", received.len());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let name = args.next();
    let server = if name.is_none() {
        Some(echo::Server::start()?)
    } else {
        None
    };
    let builder = match name {
        Some(name) => {
            TcpConnectRequest::hostname(name, args.next().ok_or("expected PORT")?.parse()?)
        }
        None => TcpConnectRequest::literal(server.as_ref().ok_or("missing echo server")?.address()),
    };
    let request = builder
        .connect_timeout(Duration::from_secs(5))
        .read_inactivity_timeout(Duration::from_secs(10))
        .write_inactivity_timeout(Duration::from_secs(10))
        .send_queue_bytes(8)
        .receive_queue_bytes(8)
        .build()?;
    let engine = Engine::builder().build()?;
    let result = exchange(&engine, request);
    engine.shutdown()?;
    if let Some(server) = server {
        server.stop()?;
    }
    result
}
