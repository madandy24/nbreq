//! Explicit single-message SMTP sender. This example has no recipient or server defaults.

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::time::Duration;

use nbreq::{Engine, EngineConfig, TcpConnectRequest, TlsOptions};
use nbreq_smtp::{Delivery, Envelope, SendRequest, SmtpClient, SmtpServer, TlsPolicy};

fn usage() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "usage: send_message HOST PORT starttls|implicit EHLO_NAME FROM TO MESSAGE_FILE",
    )
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("submission could not start: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<u8, Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let host = args
        .next()
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| usage())?;
    let port: u16 = args
        .next()
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| usage())?
        .parse()?;
    let policy = match args
        .next()
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| usage())?
        .as_str()
    {
        "starttls" => TlsPolicy::RequiredStartTls,
        "implicit" => TlsPolicy::Implicit,
        _ => return Err(usage().into()),
    };
    let ehlo_name = args
        .next()
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| usage())?;
    let from = args
        .next()
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| usage())?;
    let to = args
        .next()
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| usage())?;
    let path = args.next().ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage().into());
    }

    let mut message = Vec::new();
    File::open(path)?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut message)?;
    if message.len() > 8 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MESSAGE_FILE exceeds the 8 MiB SMTP message limit",
        )
        .into());
    }
    if !message.ends_with(b"\r\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MESSAGE_FILE must end in CRLF to preserve its exact prepared bytes",
        )
        .into());
    }
    // Vec growth can leave capacity above the crate's 8 MiB retained-input cap.
    let message = message.into_boxed_slice().into_vec();
    let connect = match host.parse::<IpAddr>() {
        Ok(address) => TcpConnectRequest::literal(SocketAddr::new(address, port)),
        Err(_) => TcpConnectRequest::hostname(host.clone(), port),
    }
    .connect_timeout(Duration::from_secs(10))
    .read_inactivity_timeout(Duration::from_secs(30))
    .write_inactivity_timeout(Duration::from_secs(30))
    .send_queue_bytes(16 * 1024)
    .receive_queue_bytes(16 * 1024)
    .build()?;
    let server = SmtpServer::new(connect, TlsOptions::new(host.clone())?, policy);
    let request = SendRequest::new(server, ehlo_name, Envelope::new(from, vec![to])?, message)?;

    let engine = Engine::new(EngineConfig::spawned())?;
    let result = SmtpClient::new(&engine).send_blocking(request);
    if let Err(error) = engine.shutdown() {
        eprintln!("Engine shutdown: {error}");
    }
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("submission failed to start: {error} ({:?})", error.kind());
            if let Some(diagnostic) = error.transport_diagnostic() {
                eprintln!("transport diagnostic: {diagnostic:?}");
            }
            return Ok(1);
        }
    };
    for recipient in &outcome.recipients {
        println!("recipient {}: {:?}", recipient.address, recipient.status);
    }
    let code = match outcome.delivery {
        Delivery::Accepted { reply } => {
            println!(
                "server accepted message: SMTP {} {}",
                reply.code,
                reply.text.escape_debug()
            );
            0
        }
        Delivery::Rejected { stage, reply } => {
            println!(
                "server rejected message at {stage:?}: SMTP {} {}",
                reply.code,
                reply.text.escape_debug()
            );
            2
        }
        Delivery::NotAccepted { stage, reason } => {
            println!("message was not accepted at {stage:?}: {reason:?}");
            3
        }
        Delivery::Uncertain { stage, reason } => {
            println!(
                "acceptance is uncertain at {stage:?}: {reason:?}; do not retry automatically"
            );
            4
        }
    };
    if let Some(diagnostic) = outcome.transport_diagnostic {
        println!("transport diagnostic: {diagnostic:?}");
    }
    Ok(code)
}
