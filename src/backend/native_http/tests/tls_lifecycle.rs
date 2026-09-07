use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection};

use super::super::{NativeHttpFactory, NativeTlsConfigs};
use crate::{Completion, Engine, EngineConfig, Request, WaitOutcome};

// Send a real server certificate flight, then observe the raw socket. No response or TLS
// shutdown is necessary: these tests specifically require NBReq to close during verification.
struct CertificatePeer {
    address: SocketAddr,
    certificate: Vec<u8>,
    closed: mpsc::Receiver<()>,
    flight_sent: mpsc::Receiver<()>,
    server: thread::JoinHandle<()>,
}

impl CertificatePeer {
    fn new() -> Self {
        Self::with_fin(false)
    }

    fn with_fin(close_after_flight: bool) -> Self {
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
        let address = listener.local_addr().expect("fixture address");
        let (closed_tx, closed) = mpsc::channel();
        let (flight_tx, flight_sent) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("fixture accept");
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");
            let mut tls = ServerConnection::new(Arc::new(config)).expect("server TLS");
            while !tls.wants_write() {
                assert_ne!(tls.read_tls(&mut socket).expect("ClientHello"), 0);
                tls.process_new_packets().expect("process ClientHello");
            }
            while tls.wants_write() {
                tls.write_tls(&mut socket).expect("certificate flight");
            }
            socket.flush().expect("flush certificate flight");
            if close_after_flight {
                socket
                    .shutdown(std::net::Shutdown::Write)
                    .expect("server FIN");
            }
            let _ = flight_tx.send(());
            let mut bytes = [0; 4096];
            loop {
                match socket.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("socket failed to close: {error}"),
                }
            }
            let _ = closed_tx.send(());
        });
        Self {
            address,
            certificate: der,
            closed,
            flight_sent,
            server,
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
}

#[test]
fn tls_verification_cancel_closes_socket_before_verifier_returns() {
    let peer = CertificatePeer::new();
    let (engine, entered, release) = peer.engine();
    let pending = engine
        .client()
        .submit(
            Request::post(format!("https://{}/", peer.address))
                .body(vec![7; 512 * 1024])
                .build()
                .expect("request"),
        )
        .expect("submit");
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("verifier entered");
    pending.handle().cancel().expect("cancel");
    let closed_while_gated = peer.closed.recv_timeout(Duration::from_millis(500)).is_ok();
    release.send(()).expect("release verification");
    assert!(matches!(pending.wait(), Completion::Cancelled));
    engine.shutdown().expect("joined shutdown");
    peer.server.join().expect("peer joined");
    assert!(
        closed_while_gated,
        "cancelled request retained its socket behind verification"
    );
}

#[test]
fn tls_verification_timeout_closes_socket_before_verifier_returns() {
    let peer = CertificatePeer::new();
    let (engine, entered, release) = peer.engine();
    let pending = engine
        .client()
        .submit(
            Request::get(format!("https://{}/", peer.address))
                .total_timeout(Duration::from_millis(300))
                .build()
                .expect("request"),
        )
        .expect("submit");
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("verifier entered");
    let observed = pending.wait_for(Duration::from_millis(600));
    let timed_out_while_gated = matches!(&observed, WaitOutcome::Completed(Completion::Failed(error)) if error.kind() == crate::ErrorKind::Timeout);
    let closed_while_gated = peer.closed.recv_timeout(Duration::from_millis(200)).is_ok();
    release.send(()).expect("release verification");
    let completion = match observed {
        WaitOutcome::Completed(completion) => completion,
        WaitOutcome::TimedOut(pending) => pending.wait(),
    };
    engine.shutdown().expect("joined shutdown");
    peer.server.join().expect("peer joined");
    assert!(
        matches!(completion, Completion::Failed(error) if error.kind() == crate::ErrorKind::Timeout)
    );
    assert!(
        timed_out_while_gated,
        "total timeout waited for certificate verification"
    );
    assert!(
        closed_while_gated,
        "timed-out request retained its socket behind verification"
    );
}

#[test]
fn tls_verification_shutdown_closes_socket_then_joins_verifier() {
    let peer = CertificatePeer::new();
    let (engine, entered, release) = peer.engine();
    let pending = engine
        .client()
        .submit(
            Request::get(format!("https://{}/", peer.address))
                .build()
                .expect("request"),
        )
        .expect("submit");
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("verifier entered");
    let (stopped_tx, stopped) = mpsc::channel();
    let shutdown = thread::spawn(move || {
        let result = engine.shutdown();
        let _ = stopped_tx.send(());
        result
    });
    let closed_while_gated = peer.closed.recv_timeout(Duration::from_millis(500)).is_ok();
    let joined_too_soon = stopped.try_recv().is_ok();
    release.send(()).expect("release verification");
    shutdown
        .join()
        .expect("shutdown thread joined")
        .expect("Engine joined");
    peer.server.join().expect("peer joined");
    assert!(matches!(pending.wait(), Completion::Cancelled));
    assert!(
        closed_while_gated,
        "shutdown left the socket open behind verification"
    );
    assert!(!joined_too_soon, "shutdown detached a live verifier");
}

fn plain_peer() -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("plain listener");
    let address = listener.local_addr().expect("plain address");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("plain accept");
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("plain timeout");
        let mut request = Vec::new();
        let mut bytes = [0; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let n = socket.read(&mut bytes).expect("plain read");
            assert_ne!(n, 0);
            request.extend_from_slice(&bytes[..n]);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .expect("plain response");
    });
    (address, server)
}

#[test]
fn tls_verification_saturation_preserves_http_progress_and_queued_cancellation() {
    let peers = (0..8).map(|_| CertificatePeer::new()).collect::<Vec<_>>();
    let (entered_tx, entered) = mpsc::channel();
    let mut releases = Vec::new();
    let mut gates = Vec::new();
    for _ in 0..2 {
        let (tx, rx) = mpsc::channel();
        releases.push(tx);
        gates.push((entered_tx.clone(), rx));
    }
    let config = EngineConfig::spawned();
    let mut factory = NativeHttpFactory::new(&config);
    factory.tls = Some(
        NativeTlsConfigs::with_test_root_and_verification_gates(
            peers[0].certificate.clone().into(),
            gates,
        )
        .expect("gated TLS"),
    );
    let engine = Engine::with_spawned_factory(config, Box::new(factory)).expect("Engine");
    let mut pending = Vec::new();
    for (index, peer) in peers.iter().enumerate() {
        pending.push(
            engine
                .client()
                .submit(
                    Request::get(format!("https://{}/", peer.address))
                        .build()
                        .expect("request"),
                )
                .expect("submit"),
        );
        if index < 2 {
            entered
                .recv_timeout(Duration::from_secs(2))
                .expect("worker gated");
        }
    }
    for peer in &peers {
        peer.flight_sent
            .recv_timeout(Duration::from_secs(2))
            .expect("certificate sent");
    }
    let (address, plain) = plain_peer();
    let plain_pending = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("plain request"),
        )
        .expect("plain submit");
    let observation = plain_pending.wait_for(Duration::from_millis(500));
    let progressed = matches!(&observation, WaitOutcome::Completed(Completion::Completed(response)) if response.body() == b"ok");
    let handshakes_waited = pending.iter().all(|pending| !pending.is_complete());
    for request in &pending {
        request.handle().cancel().expect("cancel");
    }
    let sockets_closed = peers
        .iter()
        .all(|peer| peer.closed.recv_timeout(Duration::from_millis(500)).is_ok());
    for release in releases {
        let _ = release.send(());
    }
    let _plain_completion = match observation {
        WaitOutcome::Completed(completion) => completion,
        WaitOutcome::TimedOut(pending) => pending.wait(),
    };
    engine.shutdown().expect("joined shutdown");
    for peer in peers {
        peer.server.join().expect("peer joined");
    }
    plain.join().expect("plain peer joined");
    assert!(progressed, "saturated TLS workers blocked HTTP");
    assert!(
        handshakes_waited,
        "a handshake escaped the saturated worker limit"
    );
    assert!(
        sockets_closed,
        "cancellation waited for saturated verifiers"
    );
    for request in pending {
        assert!(matches!(request.wait(), Completion::Cancelled));
    }
}

#[test]
fn tls_verification_manual_drive_remains_responsive_while_worker_is_gated() {
    let peer = CertificatePeer::new();
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let config = EngineConfig::manual();
    let mut factory = NativeHttpFactory::new(&config);
    factory.tls = Some(
        NativeTlsConfigs::with_test_root_and_verification_gate(
            peer.certificate.clone().into(),
            entered_tx,
            release_rx,
        )
        .expect("gated TLS"),
    );
    let mut engine = Engine::with_backend(config, factory.into_backend().expect("backend"))
        .expect("manual Engine");
    let pending = engine
        .client()
        .submit(
            Request::get(format!("https://{}/", peer.address))
                .build()
                .expect("request"),
        )
        .expect("submit");
    assert!(
        entered.try_recv().is_err(),
        "submission drove the passive Engine"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while entered.try_recv().is_err() {
        assert!(
            Instant::now() < deadline,
            "manual handshake did not enter verification"
        );
        engine
            .drive(Instant::now() + Duration::from_millis(10))
            .expect("drive handshake");
    }
    let (address, plain) = plain_peer();
    let plain_pending = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/"))
                .total_timeout(Duration::from_secs(1))
                .build()
                .expect("plain request"),
        )
        .expect("plain submit");
    let progress = engine.drive_until(plain_pending).expect("manual progress");
    pending.handle().cancel().expect("cancel");
    engine.drive(Instant::now()).expect("reap cancellation");
    let closed_while_gated = peer.closed.recv_timeout(Duration::from_millis(500)).is_ok();
    release.send(()).expect("release");
    engine.shutdown().expect("manual shutdown");
    peer.server.join().expect("peer joined");
    plain.join().expect("plain joined");
    assert!(matches!(progress, Completion::Completed(response) if response.body() == b"ok"));
    assert!(closed_while_gated);
    assert!(matches!(pending.wait(), Completion::Cancelled));
}

#[test]
fn tls_verification_preserves_certificate_error_when_flight_precedes_fin() {
    let peer = CertificatePeer::with_fin(true);
    let key = KeyPair::generate().expect("unrelated key");
    let mut root_params =
        CertificateParams::new(vec!["other.test".into()]).expect("root parameters");
    root_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Unrelated root");
    let unrelated_root = root_params.self_signed(&key).expect("unrelated root");
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let config = EngineConfig::spawned();
    let mut factory = NativeHttpFactory::new(&config);
    factory.tls = Some(
        NativeTlsConfigs::with_test_root_and_verification_gate(
            unrelated_root.der().clone(),
            entered_tx,
            release_rx,
        )
        .expect("gated TLS"),
    );
    let engine = Engine::with_spawned_factory(config, Box::new(factory)).expect("Engine");
    let pending = engine
        .client()
        .submit(
            Request::get(format!("https://{}/", peer.address))
                .build()
                .expect("request"),
        )
        .expect("submit");
    let verification_entered = entered.recv_timeout(Duration::from_secs(2)).is_ok();
    // Leave time for FIN to be observed while the verifier owns the received flight.
    let observation = pending.wait_for(Duration::from_millis(100));
    let premature = matches!(&observation, WaitOutcome::Completed(_));
    let _ = release.send(());
    let completion = match observation {
        WaitOutcome::Completed(completion) => completion,
        WaitOutcome::TimedOut(pending) => pending.wait(),
    };
    engine.shutdown().expect("shutdown");
    peer.server.join().expect("peer joined");
    assert!(
        verification_entered && !premature,
        "FIN bypassed processing of the preceding certificate flight"
    );
    assert!(
        matches!(&completion, Completion::Failed(error) if error.tls_failure() == Some(crate::TlsFailure::CertificateUnknownIssuer)),
        "{completion:?}"
    );
}
