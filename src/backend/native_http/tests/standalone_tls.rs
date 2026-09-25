//! Directed standalone TLS owner races. The verifier gate holds an actual certificate check
//! while the network owner must close the socket and settle the canonical completion.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection};

use super::super::{
    ConnectionLimits, NativeEvent, NativeHttpBackend, NativeHttpFactory, NativeTlsConfigs,
    StandaloneTcp, StandaloneTlsTcp,
};
use crate::backend::native_tls::StandaloneTls;
use crate::registry::TlsConnectSink;
use crate::tcp::io::{TcpIoConfig, TcpIoShared};
use crate::{
    Engine, EngineConfig, ErrorKind, Request, RequestId, RunMode, TcpConnectRequest, TcpConnection,
    TcpConnectionHandle, TcpConnector, TcpStreamError, TlsConnectCompletion, TlsConnectWaitOutcome,
    TlsFailure, TlsOptions, TransportStage,
};

struct GatedCertificatePeer {
    address: SocketAddr,
    certificate: Vec<u8>,
    closed: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

impl GatedCertificatePeer {
    fn new() -> Self {
        let key = KeyPair::generate().expect("fixture key");
        let certificate = CertificateParams::new(vec!["127.0.0.1".into()])
            .expect("fixture parameters")
            .self_signed(&key)
            .expect("fixture certificate");
        let der = certificate.der().to_vec();
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_no_client_auth()
                .with_single_cert(
                    vec![certificate.der().clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .expect("server identity");
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let (closed_tx, closed) = mpsc::channel();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut socket = loop {
                if server_stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return;
                }
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            socket.set_nonblocking(false).expect("accepted socket mode");
            socket
                .set_read_timeout(Some(Duration::from_millis(100)))
                .expect("bounded read");
            socket
                .set_write_timeout(Some(Duration::from_millis(100)))
                .expect("bounded write");
            let mut tls = ServerConnection::new(Arc::new(config)).expect("server TLS");
            while !tls.wants_write() {
                if server_stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return;
                }
                match tls.read_tls(&mut socket) {
                    Ok(0) => return,
                    Ok(_) => tls.process_new_packets().expect("ClientHello"),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => panic!("ClientHello read: {error}"),
                };
            }
            while tls.wants_write() {
                if server_stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return;
                }
                tls.write_tls(&mut socket).expect("certificate flight");
            }
            socket.flush().expect("flush certificate flight");
            let mut bytes = [0_u8; 4096];
            loop {
                if server_stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return;
                }
                match socket.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("socket close observation: {error}"),
                }
            }
            let _ = closed_tx.send(());
        });
        Self {
            address,
            certificate: der,
            closed,
            stop,
            server: Some(server),
        }
    }

    fn engine(&self) -> (Engine, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let config = EngineConfig::spawned();
        let mut factory = NativeHttpFactory::new(&config);
        factory.tls = Some(
            NativeTlsConfigs::with_test_root_and_verification_gate(
                self.certificate.clone().into(),
                entered_tx,
                release_rx,
            )
            .expect("gated TLS configuration"),
        );
        let engine = Engine::with_spawned_factory(config, Box::new(factory)).expect("Engine");
        (engine, entered, release)
    }

    fn request(&self) -> crate::TcpConnectRequest {
        TcpConnectRequest::literal(self.address)
            .connect_timeout(Duration::from_secs(2))
            .build()
            .expect("literal TLS request")
    }
}

impl Drop for GatedCertificatePeer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(server) = self.server.take() {
            server.join().expect("bounded certificate peer joins");
        }
    }
}

#[test]
fn standalone_cancel_closes_socket_before_verifier_returns() {
    let peer = GatedCertificatePeer::new();
    let (engine, entered, release) = peer.engine();
    let pending = engine
        .tcp_connector()
        .submit_tls(
            peer.request(),
            TlsOptions::new("127.0.0.1").expect("TLS name"),
        )
        .expect("submit TLS");
    entered
        .recv_timeout(Duration::from_secs(3))
        .expect("verifier entered");
    pending.handle().cancel().expect("cancel TLS");
    let closed_while_gated = peer.closed.recv_timeout(Duration::from_secs(1)).is_ok();
    release.send(()).expect("release verifier");
    let outcome = pending.wait_for(Duration::from_secs(3));
    assert!(
        matches!(
            outcome,
            TlsConnectWaitOutcome::Completed(TlsConnectCompletion::Cancelled)
        ),
        "explicit cancel must settle gated standalone TLS: {outcome:?}"
    );
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    engine.shutdown().expect("joined shutdown");
    assert!(
        closed_while_gated,
        "cancel retained its socket behind verification"
    );
}

#[test]
fn standalone_timeout_closes_socket_before_verifier_returns() {
    let peer = GatedCertificatePeer::new();
    let (engine, entered, release) = peer.engine();
    let pending = engine
        .tcp_connector()
        .submit_tls(
            peer.request(),
            TlsOptions::new("127.0.0.1")
                .expect("TLS name")
                .handshake_timeout(Duration::from_millis(300)),
        )
        .expect("submit TLS");
    entered
        .recv_timeout(Duration::from_secs(3))
        .expect("verifier entered");
    let outcome = pending.wait_for(Duration::from_secs(2));
    let closed_while_gated = peer.closed.recv_timeout(Duration::from_secs(1)).is_ok();
    release.send(()).expect("release verifier");
    match outcome {
        TlsConnectWaitOutcome::Completed(TlsConnectCompletion::Failed(error)) => {
            assert_eq!(error.kind(), ErrorKind::Timeout);
        }
        other => panic!("timeout must settle while verifier is gated: {other:?}"),
    }
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    engine.shutdown().expect("joined shutdown");
    assert!(
        closed_while_gated,
        "timeout retained its socket behind verification"
    );
}

#[test]
fn standalone_shutdown_closes_socket_then_joins_verifier() {
    let peer = GatedCertificatePeer::new();
    let (engine, entered, release) = peer.engine();
    let pending = engine
        .tcp_connector()
        .submit_tls(
            peer.request(),
            TlsOptions::new("127.0.0.1").expect("TLS name"),
        )
        .expect("submit TLS");
    entered
        .recv_timeout(Duration::from_secs(3))
        .expect("verifier entered");
    let (stopped_tx, stopped) = mpsc::channel();
    let shutdown = thread::spawn(move || {
        let result = engine.shutdown();
        let _ = stopped_tx.send(());
        result
    });
    let closed_while_gated = peer.closed.recv_timeout(Duration::from_secs(1)).is_ok();
    assert!(
        stopped.try_recv().is_err(),
        "shutdown detached a live verifier"
    );
    release.send(()).expect("release verifier");
    shutdown
        .join()
        .expect("shutdown thread joined")
        .expect("Engine joined");
    let outcome = pending.wait_for(Duration::from_secs(3));
    match outcome {
        TlsConnectWaitOutcome::Completed(TlsConnectCompletion::Failed(error)) => {
            assert_eq!(error.kind(), ErrorKind::EngineStopped);
        }
        other => panic!("shutdown must stop gated standalone TLS: {other:?}"),
    }
    assert!(
        closed_while_gated,
        "shutdown retained its socket behind verification"
    );
}

#[test]
fn stalled_standalone_verification_does_not_stall_a_plain_http_request() {
    let peer = GatedCertificatePeer::new();
    let (engine, entered, release) = peer.engine();
    let pending = engine
        .tcp_connector()
        .submit_tls(
            peer.request(),
            TlsOptions::new("127.0.0.1").expect("TLS name"),
        )
        .expect("submit TLS");
    entered
        .recv_timeout(Duration::from_secs(3))
        .expect("TLS verifier entered");

    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture listener");
    listener.set_nonblocking(true).expect("bounded accept");
    let address = listener.local_addr().expect("HTTP fixture address");
    let responder = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            assert!(Instant::now() < deadline, "HTTP accept timed out");
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("HTTP accept: {error}"),
            }
        };
        socket
            .set_nonblocking(false)
            .expect("blocking accepted socket");
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("bounded HTTP read");
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("bounded HTTP write");
        let mut request = [0_u8; 1024];
        assert!(socket.read(&mut request).expect("HTTP request") > 0);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .expect("HTTP response");
    });
    let request = Request::get(format!("http://{address}/alongside-tls"))
        .connect_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(3))
        .build()
        .expect("HTTP request build");
    let http_result = engine.client().execute(request);
    pending.handle().cancel().expect("cancel stalled TLS");
    release.send(()).expect("release verifier");
    let tls_result = pending.wait_for(Duration::from_secs(3));
    engine.shutdown().expect("joined Engine shutdown");
    responder.join().expect("bounded HTTP responder joined");
    assert!(
        matches!(
            tls_result,
            TlsConnectWaitOutcome::Completed(TlsConnectCompletion::Cancelled)
        ),
        "stalled TLS cancellation settles: {tls_result:?}"
    );
    let response = http_result.expect("unrelated HTTP request progresses during TLS verification");
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"ok");
}

#[test]
fn observed_peer_fin_is_deferred_until_inflight_tls_worker_result() {
    let key = KeyPair::generate().expect("fixture key");
    let certificate = CertificateParams::new(vec!["127.0.0.1".into()])
        .expect("fixture certificate parameters")
        .self_signed(&key)
        .expect("fixture certificate");
    let configs =
        NativeTlsConfigs::with_test_root(certificate.der().clone()).expect("fixture client trust");
    let mut client = configs
        .standalone_connection("127.0.0.1")
        .expect("unverified client session");
    assert!(!client.start().expect("ClientHello").is_empty());
    let (engine, mut backend, slot, connection, mut live, mut socket) = owner_fixture(client);
    let shared = engine.shared_for_testing();
    let request = TcpConnectRequest::literal(socket.local_addr().expect("fixture address"))
        .build()
        .expect("fixture TLS request");
    let state = shared
        .accept_tls_connect(
            engine.tcp_connector(),
            request,
            TlsOptions::new("127.0.0.1").expect("TLS name"),
            None,
        )
        .expect("pending TLS completion");
    live.sink = Some(TlsConnectSink::new(&shared, Arc::clone(&state)));
    let session = live
        .tls
        .take_handshake()
        .expect("worker owns handshake session");
    backend
        .tls_workers
        .submit(
            slot,
            session,
            crate::body_budget::BodyBuffer::default(),
            None,
        )
        .expect("submit incomplete handshake to real worker");
    backend.standalone_tls_live.insert(slot, live);
    assert!(
        backend
            .handle_standalone_tls_event(&NativeEvent::PeerClosed(slot), false)
            .expect("owner records peer FIN while worker owns session")
    );
    assert!(
        backend
            .standalone_tls_live
            .get(&slot)
            .is_some_and(|live| live.peer_fin && live.tls.handshake_in_worker()),
        "peer FIN must remain attached to the in-flight handshake"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.standalone_tls_live.contains_key(&slot) && Instant::now() < deadline {
        backend
            .service_tls(&mut Vec::new())
            .expect("apply worker result");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !backend.standalone_tls_live.contains_key(&slot),
        "deferred peer FIN must fail the incomplete worker result"
    );
    match state.wait_for(Duration::from_secs(1)) {
        Some(TlsConnectCompletion::Failed(error)) => {
            assert_eq!(error.transport_stage(), Some(TransportStage::Tls));
            assert_eq!(error.tls_failure(), Some(TlsFailure::Truncated));
        }
        other => panic!("deferred peer FIN must fail establishment: {other:?}"),
    }
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("bounded close observation");
    assert_eq!(socket.read(&mut [0_u8; 1]).expect("socket closure"), 0);
    drop(connection);
    engine.shutdown().expect("test Engine shutdown");
}

fn completed_tls_pair(
    version: &'static rustls::SupportedProtocolVersion,
    expected: rustls::ProtocolVersion,
) -> (StandaloneTls, ServerConnection) {
    let key = KeyPair::generate().expect("TLS 1.2 key");
    let certificate = CertificateParams::new(vec!["127.0.0.1".into()])
        .expect("TLS 1.2 certificate parameters")
        .self_signed(&key)
        .expect("TLS 1.2 certificate");
    let client_config =
        NativeTlsConfigs::with_test_root(certificate.der().clone()).expect("TLS 1.2 client trust");
    let mut client = client_config
        .standalone_connection("127.0.0.1")
        .expect("TLS 1.2 client session");
    let server_config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[version])
            .expect("selected TLS version")
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .expect("TLS 1.2 server identity");
    let mut server = ServerConnection::new(Arc::new(server_config)).expect("TLS 1.2 server");
    let mut client_flight = client.start().expect("ClientHello");
    for _ in 0..8 {
        if !client_flight.is_empty() {
            let mut input = std::io::Cursor::new(&client_flight);
            while (input.position() as usize) < client_flight.len() {
                assert_ne!(server.read_tls(&mut input).expect("server record input"), 0);
                server
                    .process_new_packets()
                    .expect("server handshake flight");
            }
        }
        let mut server_flight = Vec::new();
        while server.wants_write() {
            server
                .write_tls(&mut server_flight)
                .expect("server TLS output");
        }
        client_flight = if server_flight.is_empty() {
            Vec::new()
        } else {
            client
                .receive(&server_flight)
                .expect("client handshake flight")
                .outbound
        };
        if !client.is_handshaking() && !server.is_handshaking() && client_flight.is_empty() {
            assert_eq!(client.protocol_version(), Some(expected));
            return (client, server);
        }
    }
    panic!("in-memory TLS 1.2 handshake did not finish in eight flights");
}

fn completed_tls12_pair() -> (StandaloneTls, ServerConnection) {
    completed_tls_pair(&rustls::version::TLS12, rustls::ProtocolVersion::TLSv1_2)
}

fn completed_tls13_pair() -> (StandaloneTls, ServerConnection) {
    completed_tls_pair(&rustls::version::TLS13, rustls::ProtocolVersion::TLSv1_3)
}

fn tls12_close_progress(
    client: &mut StandaloneTls,
    server: &mut ServerConnection,
) -> super::super::TlsProgress {
    let close = tls12_close_record(server);
    let progress = client
        .receive(&close)
        .expect("authenticated TLS 1.2 close_notify");
    assert!(progress.peer_closed);
    progress
}

fn tls12_close_record(server: &mut ServerConnection) -> Vec<u8> {
    server.send_close_notify();
    let mut close = Vec::new();
    while server.wants_write() {
        server.write_tls(&mut close).expect("server close_notify");
    }
    close
}

fn owner_fixture(
    tls: StandaloneTls,
) -> (
    Engine,
    NativeHttpBackend,
    super::super::SlotId,
    TcpConnection,
    StandaloneTlsTcp,
    std::net::TcpStream,
) {
    let engine = Engine::new(EngineConfig::manual()).expect("manual test Engine");
    let shared = engine.shared_for_testing();
    let request_id = RequestId {
        engine: shared.id,
        sequence: 7,
    };
    let listener = TcpListener::bind("127.0.0.1:0").expect("reactor fixture listener");
    let address = listener.local_addr().expect("reactor fixture address");
    let config = EngineConfig::manual();
    let mut backend = NativeHttpBackend::new(
        super::LIMITS,
        None,
        None,
        ConnectionLimits::from_config(&config),
    )
    .expect("native backend");
    let slot = backend
        .reactor
        .connect(address, None, 18 * 1024, 18 * 1024)
        .expect("reactor fixture connect");
    let (socket, _) = listener.accept().expect("reactor fixture accept");
    let (io, owner) = TcpIoShared::pair(TcpIoConfig {
        engine_id: shared.id,
        request_id,
        shared: Arc::clone(&shared),
        run_mode: RunMode::Manual,
        send_window: 4096,
        receive_window: 4096,
        local: "127.0.0.1:1".parse().expect("local address"),
        peer: address,
        engine_waker: None,
        on_release: Box::new(|| {}),
    });
    let connector = TcpConnector::new(shared);
    let handle = TcpConnectionHandle::new(connector, request_id);
    let connection = TcpConnection::from_shared(io, handle);
    let live = StandaloneTlsTcp {
        transport: StandaloneTcp::new(request_id, owner, None, None, Instant::now()),
        tls,
        sink: None,
        handshake_deadline: None,
        wire_pending: StandaloneTlsTcp::wire_buffer(),
        wire_offset: 0,
        retained_plaintext: Vec::new(),
        retained_offset: 0,
        plaintext_inflight: 0,
        peer_close_notify: false,
        peer_fin: false,
        local_close_notify: false,
        write_shutdown: false,
    };
    (engine, backend, slot, connection, live, socket)
}

#[test]
fn tls12_peer_close_with_queued_application_bytes_fails_before_control_output() {
    let (client, mut server) = completed_tls12_pair();
    let (engine, mut backend, slot, mut connection, mut live, _socket) = owner_fixture(client);
    connection
        .try_send(b"accepted-but-not-sent".to_vec())
        .expect("queue application output");
    let progress = tls12_close_progress(&mut live.tls, &mut server);
    let error = backend
        .apply_standalone_tls_progress(slot, &mut live, progress)
        .expect_err("TLS 1.2 peer close must fail with queued application bytes");
    assert_eq!(error.transport_stage(), Some(TransportStage::Send));
    assert_eq!(error.tls_failure(), Some(TlsFailure::Protocol));
    assert!(
        live.wire_pending.is_empty(),
        "no later control output may overtake queued app data"
    );
    assert_eq!(
        connection
            .try_send(b"late".to_vec())
            .expect_err("write admission closed atomically")
            .kind(),
        crate::TcpSendErrorKind::Closed
    );
    backend.reactor.cancel(slot);
    drop(connection);
    engine.shutdown().expect("test Engine shutdown");
}

#[test]
fn tls12_same_batch_drained_output_is_credited_before_peer_close() {
    let (client, mut server) = completed_tls12_pair();
    let (engine, mut backend, slot, mut connection, mut live, mut socket) = owner_fixture(client);
    connection
        .try_send(b"sent".to_vec())
        .expect("queue application output");
    let accepted = live
        .transport
        .owner
        .take_tls_outbound_up_to(4)
        .expect("extract accepted output");
    assert_eq!(accepted, b"sent");
    let (count, ciphertext) = live
        .tls
        .encrypt(&accepted, super::super::STANDALONE_TLS_WIRE_WINDOW)
        .expect("encrypt application record");
    assert_eq!(count, accepted.len());
    live.plaintext_inflight = count;
    backend
        .reactor
        .queue_write(slot, &ciphertext)
        .expect("queue real ciphertext on reactor");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !backend
        .reactor
        .outbound_is_empty(slot)
        .expect("reactor output state")
        && Instant::now() < deadline
    {
        // Deliberately hold this batch's WriteDrained event until after peer close handling.
        let _ = backend
            .reactor
            .poll(Instant::now() + Duration::from_millis(10))
            .expect("send real ciphertext");
    }
    assert!(
        backend
            .reactor
            .outbound_is_empty(slot)
            .expect("reactor output drained"),
        "the complete application record reached the network"
    );
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("bounded record read");
    let mut record = vec![0_u8; ciphertext.len()];
    socket
        .read_exact(&mut record)
        .expect("whole application record");
    let mut input = std::io::Cursor::new(&record);
    while (input.position() as usize) < record.len() {
        assert_ne!(server.read_tls(&mut input).expect("server record input"), 0);
        server
            .process_new_packets()
            .expect("server authenticates drained record");
    }
    let mut plaintext = [0_u8; 4];
    server
        .reader()
        .read_exact(&mut plaintext)
        .expect("server reads sent application bytes");
    assert_eq!(&plaintext, b"sent");
    let progress = tls12_close_progress(&mut live.tls, &mut server);
    backend
        .apply_standalone_tls_progress(slot, &mut live, progress)
        .expect("fully drained same-batch app output permits a close response");
    assert_eq!(live.plaintext_inflight, 0);
    assert!(live.peer_close_notify);
    assert_eq!(
        connection
            .try_send(b"late".to_vec())
            .expect_err("write admission closed after peer close")
            .kind(),
        crate::TcpSendErrorKind::Closed
    );
    backend.reactor.cancel(slot);
    drop(connection);
    engine.shutdown().expect("test Engine shutdown");
}

#[test]
fn tls12_peer_close_discards_already_encrypted_unsent_application_record() {
    let (client, mut server) = completed_tls12_pair();
    let (engine, mut backend, slot, mut connection, mut live, mut socket) = owner_fixture(client);
    let plaintext = b"encrypted but not yet put on the socket";
    connection
        .try_send(plaintext.to_vec())
        .expect("admit application output");
    let accepted = live
        .transport
        .owner
        .take_tls_outbound_up_to(plaintext.len())
        .expect("owner extracts accepted output");
    assert_eq!(accepted, plaintext);
    let (count, ciphertext) = live
        .tls
        .encrypt(&accepted, super::super::STANDALONE_TLS_WIRE_WINDOW)
        .expect("encrypt real application record");
    assert_eq!(count, plaintext.len());
    assert!(
        !ciphertext.is_empty(),
        "the record must be staged on the wire"
    );
    live.plaintext_inflight = count;
    live.queue_wire(ciphertext).expect("stage ciphertext");
    assert!(!live.wire_pending.is_empty());
    backend.standalone_tls_live.insert(slot, live);

    let close = tls12_close_record(&mut server);
    assert!(
        backend
            .handle_standalone_tls_event(
                &NativeEvent::Data(slot, crate::body_budget::BodyBuffer::from_vec(close)),
                false,
            )
            .expect("owner processes peer close")
    );
    assert!(
        !backend.standalone_tls_live.contains_key(&slot),
        "owner must discard the TLS session and staged ciphertext"
    );
    match connection.try_read(&mut [0_u8; 1]) {
        Err(TcpStreamError::Failed(error)) => {
            assert_eq!(error.transport_stage(), Some(TransportStage::Send));
            assert_eq!(error.tls_failure(), Some(TlsFailure::Protocol));
        }
        other => panic!("pending ciphertext must fail the connection: {other:?}"),
    }
    assert_eq!(
        connection
            .try_send(b"late".to_vec())
            .expect_err("failure closes write admission")
            .kind(),
        crate::TcpSendErrorKind::Closed
    );
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("bounded socket observation");
    match socket.read(&mut [0_u8; 1]) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ) => {}
        other => panic!("staged application ciphertext escaped after peer close: {other:?}"),
    }
    drop(connection);
    engine.shutdown().expect("test Engine shutdown");
}

#[test]
fn restored_tls13_control_reply_is_staged_without_application_output() {
    let (client, mut server) = completed_tls13_pair();
    let (engine, mut backend, slot, connection, mut live, mut socket) = owner_fixture(client);
    server.refresh_traffic_keys().expect("server KeyUpdate");
    let mut update = Vec::new();
    while server.wants_write() {
        server.write_tls(&mut update).expect("KeyUpdate record");
    }
    assert!(!update.is_empty());
    let session = live.tls.take_session_for_test();
    backend
        .tls_workers
        .submit(
            slot,
            session,
            crate::body_budget::BodyBuffer::from_vec(update),
            None,
        )
        .expect("authenticated control record enters the real TLS worker");
    backend.standalone_tls_live.insert(slot, live);

    // A post-handshake worker result follows the same owner restoration branch as the
    // handshake's final flight. The application writer stays idle throughout.
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend
        .standalone_tls_live
        .get(&slot)
        .is_some_and(|live| live.tls.handshake_in_worker())
        && Instant::now() < deadline
    {
        backend
            .service_tls(&mut Vec::new())
            .expect("restore worker result");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        backend
            .standalone_tls_live
            .get(&slot)
            .is_some_and(|live| !live.tls.handshake_in_worker()),
        "worker restored the verified TLS session"
    );
    assert_eq!(
        backend.standalone_tls_live[&slot]
            .transport
            .owner
            .send_occupancy(),
        0
    );
    socket
        .set_read_timeout(Some(Duration::from_millis(20)))
        .expect("bounded control read");
    let mut reply = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while reply.len() < 5 && Instant::now() < deadline {
        let _ = backend
            .reactor
            .poll(Instant::now() + Duration::from_millis(10))
            .expect("flush owner control output");
        let mut buffer = [0_u8; 1024];
        match socket.read(&mut buffer) {
            Ok(count) if count > 0 => reply.extend_from_slice(&buffer[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            other => panic!("owner did not send TLS control output: {other:?}"),
        }
    }
    assert!(
        reply.len() >= 5,
        "owner sent control traffic without app output"
    );
    let expected_record_len = 5 + usize::from(u16::from_be_bytes([reply[3], reply[4]]));
    while reply.len() < expected_record_len && Instant::now() < deadline {
        let _ = backend
            .reactor
            .poll(Instant::now() + Duration::from_millis(10))
            .expect("continue control output");
        let mut buffer = [0_u8; 1024];
        match socket.read(&mut buffer) {
            Ok(count) if count > 0 => reply.extend_from_slice(&buffer[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            other => panic!("incomplete TLS control output: {other:?}"),
        }
    }
    assert_eq!(
        reply.len(),
        expected_record_len,
        "complete encrypted control record"
    );
    let mut input = std::io::Cursor::new(&reply);
    while (input.position() as usize) < reply.len() {
        assert_ne!(
            server.read_tls(&mut input).expect("server control input"),
            0
        );
        let state = server
            .process_new_packets()
            .expect("server authenticates KeyUpdate response");
        assert!(
            !state.peer_has_closed(),
            "control reply is not close_notify"
        );
    }
    match server.reader().read(&mut [0_u8; 1]) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("control reply contained application data: {other:?}"),
    }
    server
        .writer()
        .write_all(b"updated keys work")
        .expect("server application output");
    let mut ciphertext = Vec::new();
    while server.wants_write() {
        server
            .write_tls(&mut ciphertext)
            .expect("server updated-key output");
    }
    let received = backend
        .standalone_tls_live
        .get_mut(&slot)
        .expect("live owner")
        .tls
        .receive(&ciphertext)
        .expect("client reads under updated keys");
    assert_eq!(received.plaintext.into_vec(), b"updated keys work");
    backend.reactor.cancel(slot);
    drop(connection);
    engine.shutdown().expect("test Engine shutdown");
}

#[test]
fn old_worker_result_cannot_replace_reused_slot_tls_session() {
    let (old_client, _old_server) = completed_tls13_pair();
    let (engine, mut backend, old_slot, connection, mut live, mut old_socket) =
        owner_fixture(old_client);
    let old_session = live.tls.take_session_for_test();
    backend
        .tls_workers
        .submit(
            old_slot,
            old_session,
            crate::body_budget::BodyBuffer::default(),
            None,
        )
        .expect("submit old session to worker");
    backend.standalone_tls_live.insert(old_slot, live);
    let mut live = backend
        .standalone_tls_live
        .remove(&old_slot)
        .expect("cancel old owner before its worker result");
    assert!(backend.reactor.cancel(old_slot));
    old_socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("bounded old-socket observation");
    assert_eq!(
        old_socket.read(&mut [0_u8; 1]).expect("old socket closed"),
        0
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("replacement listener");
    let replacement = backend
        .reactor
        .connect(
            listener.local_addr().expect("replacement address"),
            None,
            18 * 1024,
            18 * 1024,
        )
        .expect("replacement connect");
    let (_new_socket, _) = listener.accept().expect("replacement accepted");
    assert!(
        old_slot.same_index_for_test(replacement),
        "the replacement must reuse the same reactor index with a new generation"
    );
    let (new_client, mut new_server) = completed_tls13_pair();
    live.tls = new_client;
    backend.standalone_tls_live.insert(replacement, live);

    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.tls_workers.occupied_for_test() != 0 && Instant::now() < deadline {
        backend
            .service_tls(&mut Vec::new())
            .expect("discard old Finished");
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        backend.tls_workers.occupied_for_test(),
        0,
        "the old worker result was consumed"
    );
    let replacement_live = backend
        .standalone_tls_live
        .get_mut(&replacement)
        .expect("replacement owner survived old Finished");
    assert!(!replacement_live.tls.handshake_in_worker());
    new_server
        .writer()
        .write_all(b"new-generation")
        .expect("replacement server writes");
    let mut ciphertext = Vec::new();
    while new_server.wants_write() {
        new_server
            .write_tls(&mut ciphertext)
            .expect("replacement server record");
    }
    let progress = replacement_live
        .tls
        .receive(&ciphertext)
        .expect("replacement session still has its own authenticated keys");
    assert_eq!(progress.plaintext.into_vec(), b"new-generation");
    backend.reactor.cancel(replacement);
    drop(connection);
    engine.shutdown().expect("test Engine shutdown");
}
