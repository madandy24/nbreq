// Based on the accepted M1 fixture; R4 reaps completed owners and bounds live sockets.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::json;

use crate::{Args, Result, argument, emit};

#[derive(Default)]
struct Stats {
    connections: AtomicUsize,
    requests: AtomicUsize,
    completed: AtomicUsize,
    aborted: AtomicUsize,
    current: AtomicUsize,
    high: AtomicUsize,
    bad_body: AtomicUsize,
}

impl Stats {
    fn json(&self) -> serde_json::Value {
        json!({"connections": self.connections.load(Ordering::Acquire),
            "requests": self.requests.load(Ordering::Acquire), "completed": self.completed.load(Ordering::Acquire),
            "aborted": self.aborted.load(Ordering::Acquire), "active_requests": self.current.load(Ordering::Acquire),
            "high_active_requests": self.high.load(Ordering::Acquire), "bad_body": self.bad_body.load(Ordering::Acquire)})
    }
}

pub fn identity() -> Result<(Vec<u8>, Arc<ServerConfig>)> {
    let root_key = KeyPair::generate()?;
    let mut root_params = CertificateParams::new(Vec::<String>::new())?;
    root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    root_params
        .distinguished_name
        .push(DnType::CommonName, "NBReq R4 ephemeral private CA");
    root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    root_params.not_before = date_time_ymd(2020, 1, 1);
    root_params.not_after = date_time_ymd(2040, 1, 1);
    let root = root_params.self_signed(&root_key)?;
    let key = KeyPair::generate()?;
    let mut leaf_params = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "127.0.0.1");
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.use_authority_key_identifier_extension = true;
    // Exercise current platform policy, including Apple's EKU and certificate-lifetime rules.
    let now = time::OffsetDateTime::now_utc();
    leaf_params.not_before = now - time::Duration::days(1);
    leaf_params.not_after = now + time::Duration::days(30);
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = leaf_params.signed_by(&key, &root, &root_key)?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok((root.der().to_vec(), Arc::new(config)))
}

enum Connection {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ServerConnection, TcpStream>>),
}
impl Read for Connection {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(bytes),
            Self::Tls(stream) => stream.read(bytes),
        }
    }
}
impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(bytes),
            Self::Tls(stream) => stream.write(bytes),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

fn serve(connection: Connection, stats: &Stats, stop: &AtomicBool, corrupt: bool) -> Result<()> {
    let mut stream = BufReader::new(connection);
    let mut line = String::with_capacity(256);
    let mut bytes = [0; 8192];
    while !stop.load(Ordering::Acquire) {
        line.clear();
        if stream.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let path = line
            .split_whitespace()
            .nth(1)
            .ok_or("missing request path")?;
        let parts: Vec<_> = path.trim_matches('/').split('/').collect();
        if parts.len() != 4 || parts[0] != "body" {
            return Err("invalid fixture route".into());
        }
        let body_bytes: usize = parts[1].parse()?;
        let hold_ms: u64 = parts[2].parse()?;
        let chunk_ms: u64 = parts[3].parse()?;
        if body_bytes > 8 * 1024 * 1024 || hold_ms > 2000 || chunk_ms > 20 {
            return Err("fixture bounds exceeded".into());
        }
        let mut content_length = 0;
        let mut head_bytes = line.len();
        loop {
            line.clear();
            if stream.read_line(&mut line)? == 0 {
                return Err("EOF in request headers".into());
            }
            head_bytes += line.len();
            if head_bytes > 16384 {
                return Err("oversized fixture request headers".into());
            }
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>()?;
                }
            }
        }
        if content_length > 8 * 1024 * 1024 {
            return Err("oversized fixture upload".into());
        }
        let mut received = 0;
        while received < content_length {
            let read = (content_length - received).min(bytes.len());
            stream.read_exact(&mut bytes[..read])?;
            if bytes[..read]
                .iter()
                .enumerate()
                .any(|(index, byte)| *byte != ((received + index) % 251) as u8)
            {
                stats.bad_body.fetch_add(1, Ordering::AcqRel);
                return Err("fixture upload bytes differ".into());
            }
            received += read;
        }
        stats.requests.fetch_add(1, Ordering::AcqRel);
        let active = stats.current.fetch_add(1, Ordering::AcqRel) + 1;
        stats.high.fetch_max(active, Ordering::AcqRel);
        let sent = (|| -> Result<()> {
            thread::sleep(Duration::from_millis(hold_ms));
            write!(
                stream.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {body_bytes}\r\nContent-Type: application/octet-stream\r\n\r\n"
            )?;
            stream.get_mut().flush()?;
            let mut offset = 0;
            while offset < body_bytes {
                let size = (body_bytes - offset).min(if chunk_ms == 0 { 8192 } else { 1024 });
                for (index, byte) in bytes[..size].iter_mut().enumerate() {
                    *byte = ((offset + index) % 251) as u8;
                }
                if corrupt && offset == 0 {
                    bytes[0] ^= 1;
                }
                stream.get_mut().write_all(&bytes[..size])?;
                stream.get_mut().flush()?;
                offset += size;
                if chunk_ms > 0 {
                    thread::sleep(Duration::from_millis(chunk_ms));
                }
            }
            Ok(())
        })();
        stats.current.fetch_sub(1, Ordering::AcqRel);
        match sent {
            Ok(()) => {
                stats.completed.fetch_add(1, Ordering::AcqRel);
            }
            Err(error) => {
                stats.aborted.fetch_add(1, Ordering::AcqRel);
                return Err(error);
            }
        }
    }
    Ok(())
}

fn serve_echo(mut stream: Connection, stats: &Stats) -> Result<()> {
    let mut bytes = Vec::new();
    (&mut stream)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024
        || bytes
            .iter()
            .enumerate()
            .any(|(i, byte)| *byte != (i % 251) as u8)
    {
        stats.bad_body.fetch_add(1, Ordering::AcqRel);
        return Err("echo fixture received invalid body".into());
    }
    stats.requests.fetch_add(1, Ordering::AcqRel);
    stream.write_all(&bytes)?;
    stream.flush()?;
    stats.completed.fetch_add(1, Ordering::AcqRel);
    Ok(())
}

pub fn run(args: &Args) -> Result<()> {
    let tls = argument(args, "--tls", "no") == "yes";
    let corrupt = argument(args, "--corrupt", "no") == "yes";
    let echo = argument(args, "--echo", "no") == "yes";
    let (root, server_config) = identity()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Stats::default());
    let sockets = Arc::new(Mutex::new(HashMap::<usize, TcpStream>::new()));
    let (thread_stop, thread_stats, thread_sockets) =
        (Arc::clone(&stop), Arc::clone(&stats), Arc::clone(&sockets));
    let acceptor = thread::spawn(move || -> std::result::Result<(), String> {
        let mut handlers = Vec::<thread::JoinHandle<()>>::new();
        while !thread_stop.load(Ordering::Acquire) {
            let (stream, _) = listener.accept().map_err(|error| error.to_string())?;
            if thread_stop.load(Ordering::Acquire) {
                break;
            }
            let id = thread_stats.connections.fetch_add(1, Ordering::AcqRel);
            let mut index = 0;
            while index < handlers.len() {
                if handlers[index].is_finished() {
                    handlers
                        .swap_remove(index)
                        .join()
                        .map_err(|_| "fixture handler panicked")?;
                } else {
                    index += 1;
                }
            }
            if handlers.len() >= 64 {
                return Err("fixture live connection bound exceeded".to_owned());
            }
            stream
                .set_nodelay(true)
                .map_err(|error| error.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|error| error.to_string())?;
            stream
                .set_write_timeout(Some(Duration::from_secs(10)))
                .map_err(|error| error.to_string())?;
            thread_sockets
                .lock()
                .map_err(|_| "socket registry poisoned")?
                .insert(id, stream.try_clone().map_err(|error| error.to_string())?);
            let connection = if tls {
                Connection::Tls(Box::new(StreamOwned::new(
                    ServerConnection::new(Arc::clone(&server_config))
                        .map_err(|error| error.to_string())?,
                    stream,
                )))
            } else {
                Connection::Plain(stream)
            };
            let (stats, stop) = (Arc::clone(&thread_stats), Arc::clone(&thread_stop));
            let sockets = Arc::clone(&thread_sockets);
            handlers.push(thread::spawn(move || {
                // Disconnects are expected during cancellation and shutdown. Exact successful
                // bodies are checked by the client; malformed uploads invalidate fixture stats.
                let _outcome = if echo {
                    serve_echo(connection, &stats)
                } else {
                    serve(connection, &stats, &stop, corrupt)
                };
                sockets.lock().expect("fixture socket registry").remove(&id);
            }));
        }
        for handler in handlers {
            handler.join().map_err(|_| "fixture handler panicked")?;
        }
        Ok(())
    });
    emit(
        json!({"event":"fixture_ready", "address":address.to_string(), "root_der":root, "pid":std::process::id(), "tls":tls}),
    )?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    stop.store(true, Ordering::Release);
    for socket in sockets
        .lock()
        .map_err(|_| "socket registry poisoned")?
        .values()
    {
        let _closed = socket.shutdown(Shutdown::Both);
    }
    let _wake = TcpStream::connect(address)?;
    acceptor
        .join()
        .map_err(|_| "fixture acceptor panicked")?
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let remaining = sockets
        .lock()
        .map_err(|_| "socket registry poisoned")?
        .len();
    if remaining != 0 {
        return Err("fixture socket owners remained after join".into());
    }
    emit(
        json!({"event":"fixture_stopped", "stats":stats.json(), "joined":true, "socket_owners":remaining}),
    )?;
    Ok(())
}
