//! Bounded one-connection loopback peers for public TCP TLS tests.
//!
//! STARTTLS reads its command one byte at a time, leaving the first TLS record on the same
//! socket. Each peer has a deadline and short socket timeouts; Drop releases holds and joins it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Cursor, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ProtocolVersion, ServerConfig, ServerConnection};

const DEADLINE: Duration = Duration::from_secs(8);
const TICK: Duration = Duration::from_millis(100);

/// A test-private authority and one server identity. Use `wrong_host` for hostname rejection.
pub(crate) struct TestIdentity {
    pub(crate) root_der: Vec<u8>,
    leaf_der: CertificateDer<'static>,
    key_der: Vec<u8>,
}

impl TestIdentity {
    pub(crate) fn for_host(host: &str) -> Self {
        let ca_key = KeyPair::generate().expect("fixture CA key");
        let mut ca_params =
            CertificateParams::new(Vec::<String>::new()).expect("fixture CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "NBReq TCP TLS fixture CA");
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = ca_params
            .self_signed(&ca_key)
            .expect("fixture CA certificate");
        let key = KeyPair::generate().expect("fixture leaf key");
        let mut params =
            CertificateParams::new(vec![host.to_owned()]).expect("fixture leaf params");
        params.distinguished_name.push(DnType::CommonName, host);
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf = params
            .signed_by(&key, &ca, &ca_key)
            .expect("fixture leaf certificate");
        Self {
            root_der: ca.der().to_vec(),
            leaf_der: leaf.der().clone(),
            key_der: key.serialize_der(),
        }
    }

    pub(crate) fn localhost() -> Self {
        Self::for_host("127.0.0.1")
    }
    pub(crate) fn wrong_host() -> Self {
        Self::for_host("other.test")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Protocol {
    Tls12,
    Tls13,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Entry {
    Immediate,
    StartTls,
}

pub(crate) enum StartTlsResponse {
    Normal,
    /// Pause between the beginning and end of the 220 response.
    Fragmented,
    /// Send bytes past the line terminator, which a client must not treat as TLS input.
    Trailing(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Close {
    Notify,
    Raw,
    /// Send only part of one authenticated application record, then close raw TCP.
    RawMidRecord,
}

struct ReplyPlan<'a> {
    bytes: &'a [u8],
    close: Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Phase {
    StartTlsAckTail,
    Handshake,
    Read,
    Write,
}

pub(crate) enum Exchange {
    ReadThenSend {
        request_len: usize,
        reply: Vec<u8>,
        close: Close,
    },
    Send {
        reply: Vec<u8>,
        close: Close,
    },
    /// After TLS 1.3 handshake, request a traffic-key refresh and await an idle client reply.
    KeyUpdateIdle,
    /// Accept the socket but never send a TLS handshake flight.
    SilentHandshake,
    /// Observe an authenticated local finish, then send more TLS 1.3 application data.
    ObserveClientFinishThenSend {
        reply: Vec<u8>,
    },
    /// Initiate peer close and wait for the client's authenticated close response.
    PeerCloseThenObserve,
}

pub(crate) struct PeerOptions {
    pub(crate) entry: Entry,
    pub(crate) starttls_response: StartTlsResponse,
    pub(crate) protocol: Protocol,
    pub(crate) alpn: Vec<Vec<u8>>,
    pub(crate) exchange: Exchange,
    pub(crate) holds: BTreeSet<Phase>,
}

impl PeerOptions {
    pub(crate) fn new(exchange: Exchange) -> Self {
        Self {
            entry: Entry::Immediate,
            starttls_response: StartTlsResponse::Normal,
            protocol: Protocol::Tls13,
            alpn: Vec::new(),
            exchange,
            holds: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Event {
    Accepted,
    StartTlsReady,
    HandshakeComplete {
        version: ProtocolVersion,
        alpn: Option<Vec<u8>>,
    },
    Read(Vec<u8>),
    Wrote,
    PeerCloseNotify {
        application: Vec<u8>,
    },
    /// A valid encrypted post-handshake control flight arrived without application data.
    PostUpdateControlFlight,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Completed,
    PeerClosed,
    Stopped,
    Failed(String),
}

struct WorkerGate {
    entered: Sender<()>,
    release: Receiver<()>,
}
struct ControllerGate {
    entered: Receiver<()>,
    release: Sender<()>,
}

pub(crate) struct TestPeer {
    pub(crate) address: SocketAddr,
    events: Receiver<Event>,
    gates: BTreeMap<Phase, ControllerGate>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Outcome>>,
}

impl TestPeer {
    pub(crate) fn spawn(identity: &TestIdentity, options: PeerOptions) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("TLS fixture bind");
        listener
            .set_nonblocking(true)
            .expect("TLS fixture bounded accept");
        let address = listener.local_addr().expect("TLS fixture address");
        let version = match options.protocol {
            Protocol::Tls12 => &rustls::version::TLS12,
            Protocol::Tls13 => &rustls::version::TLS13,
        };
        let mut config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[version])
                .expect("TLS fixture version")
                .with_no_client_auth()
                .with_single_cert(
                    vec![identity.leaf_der.clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der.clone())),
                )
                .expect("TLS fixture identity");
        config.alpn_protocols = options.alpn.clone();
        let mut gates = BTreeMap::new();
        let mut worker_gates = BTreeMap::new();
        for phase in &options.holds {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            gates.insert(
                *phase,
                ControllerGate {
                    entered: entered_rx,
                    release: release_tx,
                },
            );
            worker_gates.insert(
                *phase,
                WorkerGate {
                    entered: entered_tx,
                    release: release_rx,
                },
            );
        }
        let (event_tx, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            run_peer(
                listener,
                Arc::new(config),
                options,
                worker_gates,
                event_tx,
                &worker_stop,
            )
        });
        Self {
            address,
            events,
            gates,
            stop,
            worker: Some(worker),
        }
    }

    pub(crate) fn event(&self) -> Event {
        self.events
            .recv_timeout(DEADLINE)
            .expect("TLS fixture event before deadline")
    }

    pub(crate) fn wait_held(&self, phase: Phase) {
        self.gates
            .get(&phase)
            .expect("configured fixture hold")
            .entered
            .recv_timeout(DEADLINE)
            .expect("TLS fixture reached hold");
    }

    pub(crate) fn release(&self, phase: Phase) {
        self.gates
            .get(&phase)
            .expect("configured fixture hold")
            .release
            .send(())
            .expect("release TLS fixture hold");
    }

    pub(crate) fn join(mut self) -> Outcome {
        self.worker
            .take()
            .expect("TLS fixture worker")
            .join()
            .expect("TLS fixture worker panicked")
    }
}

impl Drop for TestPeer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for gate in self.gates.values() {
            let _ = gate.release.send(());
        }
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.expect("TLS fixture worker panicked");
            }
        }
    }
}

fn run_peer(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    options: PeerOptions,
    mut gates: BTreeMap<Phase, WorkerGate>,
    events: Sender<Event>,
    stop: &AtomicBool,
) -> Outcome {
    let deadline = Instant::now() + DEADLINE;
    let mut socket = match accept(&listener, stop, deadline) {
        Ok(Some(socket)) => socket,
        Ok(None) => return Outcome::Stopped,
        Err(error) => return Outcome::Failed(format!("accept: {error}")),
    };
    let _ = events.send(Event::Accepted);
    if let Err(error) = socket.set_read_timeout(Some(TICK)) {
        return Outcome::Failed(format!("read timeout: {error}"));
    }
    if let Err(error) = socket.set_write_timeout(Some(TICK)) {
        return Outcome::Failed(format!("write timeout: {error}"));
    }
    if options.entry == Entry::StartTls {
        match starttls(
            &mut socket,
            &options.starttls_response,
            &mut gates,
            stop,
            deadline,
        ) {
            Ok(true) => {
                let _ = events.send(Event::StartTlsReady);
            }
            Ok(false) => return Outcome::Stopped,
            Err(error) => return Outcome::Failed(format!("STARTTLS: {error}")),
        }
    }
    if !hold(Phase::Handshake, &mut gates, stop, deadline) {
        return Outcome::Stopped;
    }
    if matches!(options.exchange, Exchange::SilentHandshake) {
        let mut byte = [0_u8; 1];
        return loop {
            if expired(stop, deadline) {
                break Outcome::Stopped;
            }
            match socket.read(&mut byte) {
                Ok(0) => break Outcome::PeerClosed,
                Ok(_) => {}
                Err(error) if retry(&error) => {}
                Err(error) => break Outcome::Failed(format!("silent handshake read: {error}")),
            }
        };
    }
    let mut tls = match ServerConnection::new(config) {
        Ok(tls) => tls,
        Err(error) => return Outcome::Failed(format!("server TLS: {error}")),
    };
    while tls.is_handshaking() {
        match pump(&mut tls, &mut socket, stop, deadline) {
            Ok(true) => {}
            Ok(false) => return Outcome::PeerClosed,
            Err(error) => return Outcome::Failed(format!("handshake: {error}")),
        }
    }
    let _ = events.send(Event::HandshakeComplete {
        version: tls.protocol_version().expect("completed TLS version"),
        alpn: tls.alpn_protocol().map(Vec::from),
    });
    match options.exchange {
        Exchange::ReadThenSend {
            request_len,
            reply,
            close,
        } => {
            if !hold(Phase::Read, &mut gates, stop, deadline) {
                return Outcome::Stopped;
            }
            let request = match read_plaintext(&mut tls, &mut socket, request_len, stop, deadline) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return Outcome::PeerClosed,
                Err(error) => return Outcome::Failed(format!("application read: {error}")),
            };
            let _ = events.send(Event::Read(request));
            send_reply(
                &mut tls,
                &mut socket,
                ReplyPlan {
                    bytes: &reply,
                    close,
                },
                &mut gates,
                &events,
                stop,
                deadline,
            )
        }
        Exchange::Send { reply, close } => send_reply(
            &mut tls,
            &mut socket,
            ReplyPlan {
                bytes: &reply,
                close,
            },
            &mut gates,
            &events,
            stop,
            deadline,
        ),
        Exchange::KeyUpdateIdle => key_update_idle(&mut tls, &mut socket, &events, stop, deadline),
        Exchange::ObserveClientFinishThenSend { reply } => {
            if !hold(Phase::Read, &mut gates, stop, deadline) {
                return Outcome::Stopped;
            }
            match observe_client_close(&mut tls, &mut socket, &events, stop, deadline) {
                Ok(true) => send_reply(
                    &mut tls,
                    &mut socket,
                    ReplyPlan {
                        bytes: &reply,
                        close: Close::Notify,
                    },
                    &mut gates,
                    &events,
                    stop,
                    deadline,
                ),
                Ok(false) => Outcome::PeerClosed,
                Err(error) => Outcome::Failed(format!("observe client finish: {error}")),
            }
        }
        Exchange::PeerCloseThenObserve => {
            if !hold(Phase::Write, &mut gates, stop, deadline) {
                return Outcome::Stopped;
            }
            tls.send_close_notify();
            if let Err(error) = flush(&mut tls, &mut socket, stop, deadline) {
                return Outcome::Failed(format!("peer close_notify: {error}"));
            }
            let _ = events.send(Event::Wrote);
            match observe_client_close(&mut tls, &mut socket, &events, stop, deadline) {
                Ok(true) => Outcome::Completed,
                Ok(false) => Outcome::PeerClosed,
                Err(error) => Outcome::Failed(format!("peer close response: {error}")),
            }
        }
        Exchange::SilentHandshake => unreachable!("handled before TLS construction"),
    }
}

fn accept(
    listener: &TcpListener,
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<Option<TcpStream>> {
    loop {
        if expired(stop, deadline) {
            return Ok(None);
        }
        match listener.accept() {
            Ok((socket, _)) => return Ok(Some(socket)),
            Err(error) if retry(&error) => thread::sleep(Duration::from_millis(2)),
            Err(error) => return Err(error),
        }
    }
}

fn starttls(
    socket: &mut TcpStream,
    response: &StartTlsResponse,
    gates: &mut BTreeMap<Phase, WorkerGate>,
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<bool> {
    write_raw(
        socket,
        b"220 fixture.example ESMTP ready\r\n",
        stop,
        deadline,
    )?;
    let mut command = Vec::new();
    while command.len() < 128 {
        if expired(stop, deadline) {
            return Ok(false);
        }
        let mut byte = [0_u8; 1];
        match socket.read(&mut byte) {
            Ok(0) => return Ok(false),
            Ok(1) => {
                command.push(byte[0]);
                if command.ends_with(b"\r\n") {
                    break;
                }
            }
            Ok(_) => unreachable!("one-byte read"),
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
    }
    if !command.eq_ignore_ascii_case(b"STARTTLS\r\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected command: {command:?}"),
        ));
    }
    match response {
        StartTlsResponse::Normal => {
            write_raw(socket, b"220 Ready to start TLS\r\n", stop, deadline)?
        }
        StartTlsResponse::Fragmented => {
            write_raw(socket, b"220 Ready", stop, deadline)?;
            if !hold(Phase::StartTlsAckTail, gates, stop, deadline) {
                return Ok(false);
            }
            write_raw(socket, b" to start TLS\r\n", stop, deadline)?;
        }
        StartTlsResponse::Trailing(bytes) => {
            write_raw(socket, b"220 Ready to start TLS\r\n", stop, deadline)?;
            write_raw(socket, bytes, stop, deadline)?;
        }
    }
    Ok(true)
}

fn observe_client_close(
    tls: &mut ServerConnection,
    socket: &mut TcpStream,
    events: &Sender<Event>,
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<bool> {
    let mut application = Vec::new();
    loop {
        if expired(stop, deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "client close_notify not observed",
            ));
        }
        match tls.read_tls(socket) {
            Ok(0) => return Ok(false),
            Ok(_) => {
                let state = tls.process_new_packets().map_err(io::Error::other)?;
                let mut buffer = [0_u8; 4096];
                loop {
                    match tls.reader().read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            application.extend_from_slice(&buffer[..count]);
                            if application.len() > 256 * 1024 {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "too much application data while observing close",
                                ));
                            }
                        }
                        Err(error) if retry(&error) => break,
                        Err(error) => return Err(error),
                    }
                }
                flush(tls, socket, stop, deadline)?;
                if state.peer_has_closed() {
                    let _ = events.send(Event::PeerCloseNotify { application });
                    return Ok(true);
                }
            }
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
    }
}

fn hold(
    phase: Phase,
    gates: &mut BTreeMap<Phase, WorkerGate>,
    stop: &AtomicBool,
    deadline: Instant,
) -> bool {
    let Some(gate) = gates.remove(&phase) else {
        return true;
    };
    let _ = gate.entered.send(());
    loop {
        if expired(stop, deadline) {
            return false;
        }
        match gate.release.recv_timeout(TICK) {
            Ok(()) => return !stop.load(Ordering::Acquire),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

fn read_plaintext(
    tls: &mut ServerConnection,
    socket: &mut TcpStream,
    length: usize,
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::with_capacity(length);
    let mut buffer = [0_u8; 4096];
    while bytes.len() < length {
        if expired(stop, deadline) {
            return Ok(None);
        }
        let capacity = buffer.len().min(length - bytes.len());
        match tls.reader().read(&mut buffer[..capacity]) {
            Ok(0) => {}
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count]);
                continue;
            }
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
        match tls.read_tls(socket) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                tls.process_new_packets().map_err(io::Error::other)?;
            }
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
        flush(tls, socket, stop, deadline)?;
    }
    Ok(Some(bytes))
}

fn send_reply(
    tls: &mut ServerConnection,
    socket: &mut TcpStream,
    plan: ReplyPlan<'_>,
    gates: &mut BTreeMap<Phase, WorkerGate>,
    events: &Sender<Event>,
    stop: &AtomicBool,
    deadline: Instant,
) -> Outcome {
    if !hold(Phase::Write, gates, stop, deadline) {
        return Outcome::Stopped;
    }
    if plan.close == Close::RawMidRecord {
        if plan.bytes.is_empty() || plan.bytes.len() > 16 * 1024 {
            return Outcome::Failed("mid-record fixture reply must contain 1..=16KiB bytes".into());
        }
        if let Err(error) = tls.writer().write_all(plan.bytes) {
            return Outcome::Failed(format!("mid-record plaintext write: {error}"));
        }
        let mut record = Vec::new();
        while tls.wants_write() {
            if let Err(error) = tls.write_tls(&mut record) {
                return Outcome::Failed(format!("mid-record TLS encode: {error}"));
            }
        }
        if record.len() < 6 {
            return Outcome::Failed("mid-record TLS output was too short".into());
        }
        let partial = record.len() / 2;
        if let Err(error) = write_raw(socket, &record[..partial], stop, deadline) {
            return Outcome::Failed(format!("mid-record TLS write: {error}"));
        }
        let _ = events.send(Event::Wrote);
        return Outcome::Completed;
    }
    // Keep rustls's internal output below its 64KiB limit while allowing tests to stream more
    // than 64KiB through a tiny application receive window.
    for chunk in plan.bytes.chunks(16 * 1024) {
        if let Err(error) = tls.writer().write_all(chunk) {
            return Outcome::Failed(format!("plaintext write: {error}"));
        }
        if let Err(error) = flush(tls, socket, stop, deadline) {
            return Outcome::Failed(format!("encrypted write: {error}"));
        }
    }
    let _ = events.send(Event::Wrote);
    if plan.close == Close::Notify {
        tls.send_close_notify();
        if let Err(error) = flush(tls, socket, stop, deadline) {
            return Outcome::Failed(format!("close_notify: {error}"));
        }
    }
    Outcome::Completed
}

fn key_update_idle(
    tls: &mut ServerConnection,
    socket: &mut TcpStream,
    events: &Sender<Event>,
    stop: &AtomicBool,
    deadline: Instant,
) -> Outcome {
    if tls.protocol_version() != Some(ProtocolVersion::TLSv1_3) {
        return Outcome::Failed("key update requires TLS 1.3".into());
    }
    if let Err(error) = tls.refresh_traffic_keys() {
        return Outcome::Failed(format!("key update: {error}"));
    }
    if let Err(error) = flush(tls, socket, stop, deadline) {
        return Outcome::Failed(format!("key update output: {error}"));
    }
    let mut pending = Vec::new();
    loop {
        if expired(stop, deadline) {
            return Outcome::Stopped;
        }
        let mut input = [0_u8; 4096];
        match socket.read(&mut input) {
            Ok(0) => return Outcome::PeerClosed,
            Ok(count) => pending.extend_from_slice(&input[..count]),
            Err(error) if retry(&error) => {}
            Err(error) => return Outcome::Failed(format!("key update read: {error}")),
        }
        loop {
            if pending.len() < 5 {
                break;
            }
            let length = usize::from(u16::from_be_bytes([pending[3], pending[4]]));
            if length > 18 * 1024 {
                return Outcome::Failed("oversize post-handshake TLS record".into());
            }
            let total = 5 + length;
            if pending.len() < total {
                break;
            }
            let record: Vec<u8> = pending.drain(..total).collect();
            let encrypted = record[0] == 0x17;
            if !encrypted && !(record[0] == 0x14 && length == 1) {
                return Outcome::Failed("unexpected post-handshake TLS record type".into());
            }
            let mut cursor = Cursor::new(record);
            let record_len = cursor.get_ref().len() as u64;
            while cursor.position() < record_len {
                let consumed = match tls.read_tls(&mut cursor) {
                    Ok(consumed) if consumed > 0 => consumed,
                    Ok(_) => {
                        return Outcome::Failed(
                            "rustls stopped before the full key-update record".into(),
                        );
                    }
                    Err(error) => {
                        return Outcome::Failed(format!("key update record input: {error}"));
                    }
                };
                debug_assert!(consumed <= record_len as usize);
                let state = match tls.process_new_packets() {
                    Ok(state) => state,
                    Err(error) => return Outcome::Failed(format!("key update reply: {error}")),
                };
                if state.peer_has_closed() {
                    return Outcome::PeerClosed;
                }
            }
            if cursor.position() != record_len {
                return Outcome::Failed("rustls overread the framed key-update record".into());
            }
            if encrypted {
                let mut byte = [0_u8; 1];
                match tls.reader().read(&mut byte) {
                    Ok(0) => {}
                    Err(error) if retry(&error) => {}
                    Ok(_) => {
                        return Outcome::Failed(
                            "unexpected application data after key update".into(),
                        );
                    }
                    Err(error) => return Outcome::Failed(format!("key update plaintext: {error}")),
                }
                let _ = events.send(Event::PostUpdateControlFlight);
                return send_reply(
                    tls,
                    socket,
                    ReplyPlan {
                        bytes: b"updated",
                        close: Close::Notify,
                    },
                    &mut BTreeMap::new(),
                    events,
                    stop,
                    deadline,
                );
            }
        }
    }
}

fn pump(
    tls: &mut ServerConnection,
    socket: &mut TcpStream,
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<bool> {
    flush(tls, socket, stop, deadline)?;
    if expired(stop, deadline) {
        return Ok(false);
    }
    match tls.read_tls(socket) {
        Ok(0) => Ok(false),
        Ok(_) => {
            tls.process_new_packets().map_err(io::Error::other)?;
            flush(tls, socket, stop, deadline)?;
            Ok(true)
        }
        Err(error) if retry(&error) => Ok(true),
        Err(error) => Err(error),
    }
}

fn flush(
    tls: &mut ServerConnection,
    socket: &mut TcpStream,
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    while tls.wants_write() {
        if expired(stop, deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS fixture stopped",
            ));
        }
        match tls.write_tls(socket) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "TLS fixture write made no progress",
                ));
            }
            Ok(_) => {}
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_raw(
    socket: &mut TcpStream,
    mut bytes: &[u8],
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if expired(stop, deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS fixture stopped",
            ));
        }
        match socket.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "TLS fixture greeting made no progress",
                ));
            }
            Ok(count) => bytes = &bytes[count..],
            Err(error) if retry(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn expired(stop: &AtomicBool, deadline: Instant) -> bool {
    stop.load(Ordering::Acquire) || Instant::now() >= deadline
}
fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
