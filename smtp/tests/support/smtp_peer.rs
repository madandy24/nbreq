//! Bounded, scripted SMTP loopback peer. Each expected command and DATA byte is an independent
//! wire oracle; one-byte command reads preserve the STARTTLS handshake boundary.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

const DEADLINE: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(50);

pub(crate) struct Identity {
    pub(crate) root_der: Vec<u8>,
    server: Arc<ServerConfig>,
}

impl Identity {
    pub(crate) fn for_host(host: &str) -> Self {
        let ca_key = KeyPair::generate().expect("fixture CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "NBReq SMTP fixture CA");
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
        let key = KeyPair::generate().expect("fixture leaf key");
        let mut params = CertificateParams::new(vec![host.to_owned()]).expect("leaf params");
        params.distinguished_name.push(DnType::CommonName, host);
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf = params
            .signed_by(&key, &ca, &ca_key)
            .expect("leaf certificate");
        let server =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_no_client_auth()
                .with_single_cert(
                    vec![leaf.der().clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .expect("server identity");
        Self {
            root_der: ca.der().to_vec(),
            server: Arc::new(server),
        }
    }
    pub(crate) fn localhost() -> Self {
        Self::for_host("127.0.0.1")
    }
}

pub(crate) enum Entry {
    Plain,
    ImplicitTls { expect_handshake: bool },
}
pub(crate) enum Step {
    Send(Vec<u8>),
    Delay(Duration),
    ExpectLine(Vec<u8>),
    ExpectBytes(Vec<u8>),
    Upgrade {
        expect_handshake: bool,
    },
    Hold {
        entered: Sender<()>,
        release: Receiver<()>,
    },
    ExpectClose,
    ExpectQuitOrClose,
    CloseNotify,
    Close,
}

pub(crate) struct Gate {
    entered: Receiver<()>,
    release: Sender<()>,
}
impl Gate {
    pub(crate) fn pair() -> (Self, Step) {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        (
            Self { entered, release },
            Step::Hold {
                entered: entered_tx,
                release: release_rx,
            },
        )
    }
    pub(crate) fn entered(&self) {
        self.entered
            .recv_timeout(DEADLINE)
            .expect("SMTP peer reaches gate");
    }
    pub(crate) fn try_entered(&self) -> bool {
        self.entered.try_recv().is_ok()
    }
    pub(crate) fn release(self) {
        self.release.send(()).expect("SMTP peer gate release");
    }
}

pub(crate) struct Peer {
    pub(crate) address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), String>>>,
}
impl Peer {
    pub(crate) fn spawn(identity: &Identity, entry: Entry, script: Vec<Step>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("SMTP fixture bind");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("SMTP fixture address");
        let server = Arc::clone(&identity.server);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || run(listener, server, entry, script, &worker_stop));
        Self {
            address,
            stop,
            worker: Some(worker),
        }
    }
    pub(crate) fn join(mut self) {
        self.worker
            .take()
            .expect("SMTP peer worker")
            .join()
            .expect("SMTP peer panic")
            .expect("SMTP peer script failed");
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum Wire {
    Plain(TcpStream),
    Tls(StreamOwned<ServerConnection, TcpStream>),
}
impl Read for Wire {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(bytes),
            Self::Tls(s) => s.read(bytes),
        }
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(bytes),
            Self::Tls(s) => s.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

fn run(
    listener: TcpListener,
    server: Arc<ServerConfig>,
    entry: Entry,
    script: Vec<Step>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let deadline = Instant::now() + DEADLINE;
    let socket = loop {
        if expired(stop, deadline) {
            return Err("accept deadline or stop".into());
        }
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if retry(&error) => thread::sleep(Duration::from_millis(1)),
            Err(error) => return Err(format!("SMTP accept: {error}")),
        }
    };
    socket
        .set_read_timeout(Some(TICK))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(TICK))
        .map_err(|e| e.to_string())?;
    let mut wire = match entry {
        Entry::Plain => Wire::Plain(socket),
        Entry::ImplicitTls { expect_handshake } => {
            match upgrade(socket, Arc::clone(&server), stop, deadline) {
                Ok(tls) if expect_handshake => Wire::Tls(tls),
                Ok(_) => return Err("unexpected TLS handshake success".into()),
                Err(_) if !expect_handshake => return Ok(()),
                Err(error) => return Err(format!("implicit TLS handshake: {error}")),
            }
        }
    };
    for step in script {
        if expired(stop, deadline) {
            return Err("script deadline or stop".into());
        }
        match step {
            Step::Send(bytes) => write_all(&mut wire, &bytes, stop, deadline)
                .map_err(|e| format!("SMTP send: {e}"))?,
            Step::Delay(duration) => {
                let until = Instant::now() + duration;
                while Instant::now() < until {
                    if expired(stop, deadline) {
                        return Err("SMTP delay deadline or stop".into());
                    }
                    thread::sleep(TICK.min(until.saturating_duration_since(Instant::now())));
                }
            }
            Step::ExpectLine(expected) => {
                let actual = read_line(&mut wire, stop, deadline)
                    .map_err(|e| format!("SMTP command read: {e}"))?;
                if actual != expected {
                    return Err(format!(
                        "SMTP command mismatch: expected {expected:?}, got {actual:?}"
                    ));
                }
            }
            Step::ExpectBytes(expected) => {
                let actual = read_exact_bytes(&mut wire, expected.len(), stop, deadline)
                    .map_err(|e| format!("SMTP bytes read: {e}"))?;
                if actual != expected {
                    return Err(format!(
                        "SMTP bytes mismatch: expected {expected:?}, got {actual:?}"
                    ));
                }
            }
            Step::Upgrade { expect_handshake } => {
                let Wire::Plain(socket) = wire else {
                    return Err("TLS upgrade on protected wire".into());
                };
                match upgrade(socket, Arc::clone(&server), stop, deadline) {
                    Ok(tls) if expect_handshake => wire = Wire::Tls(tls),
                    Ok(_) => return Err("unexpected STARTTLS handshake success".into()),
                    Err(_) if !expect_handshake => return Ok(()),
                    Err(error) => return Err(format!("STARTTLS handshake: {error}")),
                }
            }
            Step::Hold { entered, release } => {
                entered.send(()).map_err(|e| e.to_string())?;
                loop {
                    if expired(stop, deadline) {
                        return Err("SMTP gate deadline or stop".into());
                    }
                    match release.recv_timeout(TICK) {
                        Ok(()) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(error) => return Err(format!("SMTP gate: {error}")),
                    }
                }
            }
            Step::ExpectClose => {
                let mut byte = [0];
                loop {
                    if expired(stop, deadline) {
                        return Err("SMTP close deadline or stop".into());
                    }
                    match wire.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) => return Err("unexpected application bytes before close".into()),
                        Err(error) if retry(&error) => {}
                        Err(error) if closed(&error) => break,
                        Err(error) => return Err(format!("SMTP close read: {error}")),
                    }
                }
            }
            Step::ExpectQuitOrClose => {
                let mut first = [0];
                loop {
                    if expired(stop, deadline) { return Err("SMTP cleanup deadline or stop".into()); }
                    match wire.read(&mut first) {
                        Ok(0) => return Ok(()),
                        Ok(_) => break,
                        Err(error) if retry(&error) => {},
                        Err(error) if closed(&error) => return Ok(()),
                        Err(error) => return Err(format!("SMTP cleanup read: {error}")),
                    }
                }
                let mut command = vec![first[0]];
                while !command.ends_with(b"\r\n") && command.len() < 4096 {
                    command.extend(read_exact_bytes(&mut wire, 1, stop, deadline).map_err(|e| e.to_string())?);
                }
                if command != b"QUIT\r\n" { return Err(format!("unexpected command during cleanup: {command:?}")); }
                if let Err(error) = write_all(&mut wire, b"221 bye\r\n", stop, deadline) {
                    if !closed(&error) && error.kind() != io::ErrorKind::BrokenPipe {
                        return Err(format!("SMTP cleanup reply: {error}"));
                    }
                }
                return Ok(());
            }
            Step::CloseNotify => {
                let Wire::Tls(tls) = &mut wire else {
                    return Err("TLS close_notify on plaintext wire".into());
                };
                tls.conn.send_close_notify();
                tls.flush().map_err(|error| format!("TLS close_notify: {error}"))?;
                return Ok(());
            }
            Step::Close => return Ok(()),
        }
    }
    Ok(())
}

fn upgrade(
    socket: TcpStream,
    server: Arc<ServerConfig>,
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<StreamOwned<ServerConnection, TcpStream>> {
    let connection = ServerConnection::new(server).map_err(io::Error::other)?;
    let mut tls = StreamOwned::new(connection, socket);
    while tls.conn.is_handshaking() {
        if expired(stop, deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS deadline or stop",
            ));
        }
        match tls.conn.complete_io(&mut tls.sock) {
            Ok(_) => {}
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(tls)
}

fn read_line(wire: &mut Wire, stop: &AtomicBool, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    while line.len() < 4096 {
        line.extend(read_exact_bytes(wire, 1, stop, deadline)?);
        if line.ends_with(b"\r\n") {
            return Ok(line);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "SMTP fixture line too long",
    ))
}
fn read_exact_bytes(
    wire: &mut Wire,
    len: usize,
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; len];
    let mut offset = 0;
    while offset < len {
        if expired(stop, deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "read deadline or stop",
            ));
        }
        match wire.read(&mut bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "SMTP peer closed",
                ));
            }
            Ok(n) => offset += n,
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(bytes)
}
fn write_all(
    wire: &mut Wire,
    mut bytes: &[u8],
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if expired(stop, deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "write deadline or stop",
            ));
        }
        match wire.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "SMTP write stalled",
                ));
            }
            Ok(n) => bytes = &bytes[n..],
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
    }
    wire.flush()
}
fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
fn closed(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted)
}
fn expired(stop: &AtomicBool, deadline: Instant) -> bool {
    stop.load(Ordering::Acquire) || Instant::now() >= deadline
}
