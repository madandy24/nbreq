//! Bounded, read-only live smoke for verified IMAPS or SMTP STARTTLS.
//! Usage: `tcp-tls-probe <imaps|smtp> HOST PORT [--bundled-roots]`. No credentials or mail are sent.
//! A separate explicit `hold` mode supports client-only memory observation in the lab.

use std::error::Error;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use nbreq::{
    Engine, EngineConfig, TcpConnectRequest, TcpConnection, TcpStreamError, TlsConnection,
    TlsOptions, TlsTrust,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(40);
const MAX_LINE: usize = 2048;
const MAX_REPLY_LINES: usize = 32;

trait LineSource {
    fn read_one(&mut self, byte: &mut [u8; 1]) -> Result<Option<usize>, TcpStreamError>;
}

impl LineSource for TcpConnection {
    fn read_one(&mut self, byte: &mut [u8; 1]) -> Result<Option<usize>, TcpStreamError> {
        self.read(byte)
    }
}

impl LineSource for TlsConnection {
    fn read_one(&mut self, byte: &mut [u8; 1]) -> Result<Option<usize>, TcpStreamError> {
        self.read(byte)
    }
}

fn protocol_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn read_line(source: &mut impl LineSource, deadline: Instant) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut line = Vec::new();
    while line.len() < MAX_LINE {
        if Instant::now() >= deadline {
            return Err(io::Error::from(io::ErrorKind::TimedOut).into());
        }
        let mut byte = [0_u8; 1];
        match source.read_one(&mut byte)? {
            Some(1) => line.push(byte[0]),
            Some(_) => return Err(protocol_error("unexpected one-byte read count").into()),
            None => {
                return Err(protocol_error("peer closed before protocol reply completed").into());
            }
        }
        if line.ends_with(b"\r\n") {
            return Ok(line);
        }
    }
    Err(protocol_error("protocol reply line exceeded 2048 bytes").into())
}

fn smtp_reply(
    source: &mut impl LineSource,
    expected: u16,
    deadline: Instant,
) -> Result<Vec<Vec<u8>>, Box<dyn Error>> {
    let code = format!("{expected:03}");
    let mut lines = Vec::new();
    for _ in 0..MAX_REPLY_LINES {
        let line = read_line(source, deadline)?;
        if line.len() < 6 || &line[..3] != code.as_bytes() || !matches!(line[3], b'-' | b' ') {
            return Err(protocol_error(format!(
                "expected SMTP {expected}, got {}",
                String::from_utf8_lossy(&line).trim_end()
            ))
            .into());
        }
        let last = line[3] == b' ';
        lines.push(line);
        if last {
            return Ok(lines);
        }
    }
    Err(protocol_error("SMTP reply exceeded 32 lines").into())
}

fn imap_tagged_reply(
    source: &mut impl LineSource,
    tag: &[u8],
    deadline: Instant,
) -> Result<(bool, bool), Box<dyn Error>> {
    let mut saw_capability = false;
    let mut saw_bye = false;
    for _ in 0..MAX_REPLY_LINES {
        let line = read_line(source, deadline)?;
        if line.starts_with(b"* CAPABILITY ") {
            saw_capability = true;
        }
        if line.starts_with(b"* BYE ") {
            saw_bye = true;
        }
        if line.len() > tag.len()
            && line[..tag.len()].eq_ignore_ascii_case(tag)
            && line[tag.len()] == b' '
        {
            if !line[tag.len() + 1..].starts_with(b"OK ") {
                return Err(protocol_error(format!(
                    "IMAP tagged command failed: {}",
                    String::from_utf8_lossy(&line).trim_end()
                ))
                .into());
            }
            return Ok((saw_capability, saw_bye));
        }
    }
    Err(protocol_error("IMAP tagged reply exceeded 32 lines").into())
}

fn request(host: &str, port: u16) -> Result<nbreq::TcpConnectRequest, Box<dyn Error>> {
    Ok(TcpConnectRequest::hostname(host.to_owned(), port)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_inactivity_timeout(IDLE_TIMEOUT)
        .write_inactivity_timeout(IDLE_TIMEOUT)
        .send_queue_bytes(32 * 1024)
        .receive_queue_bytes(32 * 1024)
        .build()?)
}

fn imaps(engine: &Engine, host: &str, port: u16, deadline: Instant) -> Result<(), Box<dyn Error>> {
    let tls = TlsOptions::new(host)?.handshake_timeout(CONNECT_TIMEOUT);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(host, port)?, tls)?;
    let greeting = read_line(&mut connection, deadline)?;
    if !greeting.starts_with(b"* OK ") {
        return Err(protocol_error("IMAPS greeting was not an OK status").into());
    }
    connection.send(b"A001 CAPABILITY\r\n".to_vec())?;
    let (capability, _) = imap_tagged_reply(&mut connection, b"A001", deadline)?;
    if !capability {
        return Err(protocol_error("IMAPS CAPABILITY response was absent").into());
    }
    connection.send(b"A002 LOGOUT\r\n".to_vec())?;
    let (_, bye) = imap_tagged_reply(&mut connection, b"A002", deadline)?;
    if !bye {
        return Err(protocol_error("IMAPS LOGOUT did not include BYE").into());
    }
    println!("verified IMAPS greeting, CAPABILITY, and LOGOUT from {host}:{port}");
    Ok(())
}

fn smtp(engine: &Engine, host: &str, port: u16, deadline: Instant) -> Result<(), Box<dyn Error>> {
    let mut plain = engine.tcp_connector().execute(request(host, port)?)?;
    smtp_reply(&mut plain, 220, deadline)?;
    plain.send(b"EHLO nbreq-probe.invalid\r\n".to_vec())?;
    let capabilities = smtp_reply(&mut plain, 250, deadline)?;
    if !capabilities
        .iter()
        .any(|line| line[4..line.len() - 2].eq_ignore_ascii_case(b"STARTTLS"))
    {
        return Err(protocol_error("SMTP peer did not advertise STARTTLS").into());
    }
    plain.send(b"STARTTLS\r\n".to_vec())?;
    smtp_reply(&mut plain, 220, deadline)?;
    // The complete 220 line is consumed before moving this unsplit socket into TLS.
    let tls = TlsOptions::new(host)?.handshake_timeout(CONNECT_TIMEOUT);
    let mut secure = plain.into_tls(tls)?;
    secure.send(b"EHLO nbreq-probe.invalid\r\n".to_vec())?;
    smtp_reply(&mut secure, 250, deadline)?;
    secure.send(b"QUIT\r\n".to_vec())?;
    smtp_reply(&mut secure, 221, deadline)?;
    println!("verified SMTP STARTTLS, protected EHLO, and QUIT from {host}:{port}");
    Ok(())
}

// The TLS fixture runs in another process, so this process's RSS covers NBReq's client state.
// The phase markers let an external sampler record memory without a fragile RAM threshold.
fn hold(
    address: SocketAddr,
    name: &str,
    root_path: &str,
    count: usize,
) -> Result<(), Box<dyn Error>> {
    if !matches!(count, 16 | 32) {
        return Err("hold count must be 16 or 32".into());
    }
    let root = std::fs::read(root_path)?;
    let engine = Engine::new(EngineConfig::spawned().with_additional_tls_root_certificate(root))?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + Duration::from_secs(120);
        println!("phase=baseline count=0 reserved=0");
        io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(2));
        println!("phase=connecting count=0 reserved=0");
        io::stdout().flush()?;
        let mut connections = Vec::with_capacity(count);
        for _ in 0..count {
            if Instant::now() >= deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut).into());
            }
            let request = TcpConnectRequest::literal(address)
                .connect_timeout(Duration::from_secs(3))
                .write_inactivity_timeout(Duration::from_secs(5))
                .send_queue_bytes(16 * 1024)
                .receive_queue_bytes(16 * 1024)
                .build()?;
            let tls = TlsOptions::new(name)?.handshake_timeout(Duration::from_secs(3));
            connections.push(engine.tcp_connector().execute_tls(request, tls)?);
        }
        let current = engine.metrics().current();
        let expected_reserve = count * (16 * 1024 + 16 * 1024 + 256 * 1024);
        if current.standalone_tcp_connections() != count
            || current.reserved_tcp_queue_bytes() != expected_reserve
        {
            return Err(protocol_error("held TLS connection accounting mismatch").into());
        }
        println!(
            "phase=ready count={} reserved={}",
            current.standalone_tcp_connections(),
            current.reserved_tcp_queue_bytes()
        );
        io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(5));
        let current = engine.metrics().current();
        if current.standalone_tcp_connections() != count
            || current.reserved_tcp_queue_bytes() != expected_reserve
        {
            return Err(protocol_error("TLS sessions did not remain live during hold").into());
        }
        println!("phase=closing count={count} reserved={expected_reserve}");
        io::stdout().flush()?;
        drop(connections);
        let current = engine.metrics().current();
        if current.standalone_tcp_connections() != 0 || current.reserved_tcp_queue_bytes() != 0 {
            return Err(protocol_error("TLS permits not reclaimed after drop").into());
        }
        println!("phase=released count=0 reserved=0");
        io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(2));
        Ok(())
    })();
    let shutdown = engine.shutdown();
    result?;
    shutdown?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args
        .next()
        .ok_or("usage: tcp-tls-probe <imaps|smtp> HOST PORT | hold ADDR TLS_NAME ROOT_DER 16|32")?;
    if mode == "hold" {
        let address: SocketAddr = args
            .next()
            .ok_or("explicit literal ADDR is required")?
            .parse()?;
        let name = args.next().ok_or("explicit TLS_NAME is required")?;
        let root = args.next().ok_or("explicit ROOT_DER path is required")?;
        let count: usize = args
            .next()
            .ok_or("explicit connection count is required")?
            .parse()?;
        if args.next().is_some() {
            return Err("usage: tcp-tls-probe hold ADDR TLS_NAME ROOT_DER 16|32".into());
        }
        TlsOptions::new(name.as_str())?;
        return hold(address, &name, &root, count);
    }
    let host = args.next().ok_or("explicit HOST is required")?;
    let port: u16 = args.next().ok_or("explicit PORT is required")?.parse()?;
    let trust = match args.next().as_deref() {
        None => TlsTrust::Platform,
        Some("--bundled-roots") => TlsTrust::BundledMozilla,
        Some(_) => return Err("expected optional --bundled-roots".into()),
    };
    if args.next().is_some() || port == 0 || !matches!(mode.as_str(), "imaps" | "smtp") {
        return Err("usage: tcp-tls-probe <imaps|smtp> HOST PORT [--bundled-roots]".into());
    }
    // Reject an invalid identity before opening a network connection.
    TlsOptions::new(host.as_str())?;
    let engine = Engine::new(EngineConfig::spawned().with_tls_trust(trust))?;
    println!("TLS trust: {trust:?}");
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let result = match mode.as_str() {
        "imaps" => imaps(&engine, &host, port, deadline),
        "smtp" => smtp(&engine, &host, port, deadline),
        _ => unreachable!(),
    };
    let shutdown = engine.shutdown();
    result?;
    shutdown?;
    Ok(())
}
