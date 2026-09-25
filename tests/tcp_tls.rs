#![cfg(feature = "native")]

#[path = "support/tcp_tls_fixture.rs"]
mod fixture;

use std::time::{Duration, Instant};
use std::{sync::mpsc, thread};

use fixture::{Close, Entry, Event, Exchange, Outcome, PeerOptions, TestIdentity, TestPeer};
use nbreq::{
    Engine, EngineConfig, ErrorKind, ExecuteError, LimitKind, TcpConnectRequest, TcpRead,
    TlsConnectCompletion, TlsConnectWaitOutcome, TlsConnection, TlsFailure, TlsOptions,
};

fn request(peer: &TestPeer) -> nbreq::TcpConnectRequest {
    TcpConnectRequest::literal(peer.address)
        .connect_timeout(Duration::from_secs(2))
        .send_queue_bytes(4096)
        .receive_queue_bytes(4096)
        .build()
        .expect("fixture connect request")
}

fn verified_engine(identity: &TestIdentity) -> Engine {
    Engine::new(
        EngineConfig::spawned().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("verified Engine")
}

fn options(name: &str) -> TlsOptions {
    TlsOptions::new(name)
        .expect("TLS server identity")
        .handshake_timeout(Duration::from_secs(2))
}

fn read_tls_exact(connection: &mut TlsConnection, expected: &[u8]) {
    let mut actual = vec![0_u8; expected.len()];
    let mut used = 0;
    while used < actual.len() {
        let read = connection.read(&mut actual[used..]).expect("TLS read");
        let count = read.expect("TLS peer closed before expected bytes");
        assert!(count > 0, "TLS read made no progress");
        used += count;
    }
    assert_eq!(actual, expected);
}

fn read_plain_line(connection: &mut nbreq::TcpConnection) -> Vec<u8> {
    let mut line = Vec::new();
    while line.len() < 128 {
        let mut byte = [0_u8; 1];
        assert_eq!(
            connection.read(&mut byte).expect("plain protocol read"),
            Some(1)
        );
        line.push(byte[0]);
        if line.ends_with(b"\r\n") {
            return line;
        }
    }
    panic!("plain protocol line exceeded fixture limit");
}

fn assert_completed(peer: TestPeer) {
    let outcome = peer.join();
    assert!(
        matches!(outcome, Outcome::Completed),
        "fixture exchange did not complete: {outcome:?}"
    );
}

#[test]
fn immediate_tls_verifies_ip_identity_and_exchanges_encrypted_bytes() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::ReadThenSend {
            request_len: 4,
            reply: b"pong".to_vec(),
            close: Close::Notify,
        }),
    );
    let engine = verified_engine(&identity);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"))
        .expect("verified TLS connection");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    connection.send(b"ping".to_vec()).expect("encrypted send");
    read_tls_exact(&mut connection, b"pong");
    assert_eq!(
        connection.read(&mut [0_u8; 1]).expect("orderly TLS EOF"),
        None
    );
    assert_eq!(peer.event(), Event::Read(b"ping".to_vec()));
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn literal_endpoint_uses_explicit_dns_tls_identity_and_no_default_http_alpn() {
    let identity = TestIdentity::for_host("localhost");
    let mut peer_options = PeerOptions::new(Exchange::Send {
        reply: b"ok".to_vec(),
        close: Close::Notify,
    });
    peer_options.alpn = vec![b"http/1.1".to_vec()];
    let peer = TestPeer::spawn(&identity, peer_options);
    let engine = verified_engine(&identity);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("localhost"))
        .expect("TLS name independent of literal endpoint");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(
        peer.event(),
        Event::HandshakeComplete { alpn: None, .. }
    ));
    read_tls_exact(&mut connection, b"ok");
    assert_eq!(
        connection.read(&mut [0_u8; 1]).expect("orderly TLS EOF"),
        None
    );
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn starttls_consumes_the_unsplit_plain_connection_and_keeps_the_socket() {
    let identity = TestIdentity::localhost();
    let mut peer_options = PeerOptions::new(Exchange::ReadThenSend {
        request_len: 4,
        reply: b"secure".to_vec(),
        close: Close::Notify,
    });
    peer_options.entry = Entry::StartTls;
    let peer = TestPeer::spawn(&identity, peer_options);
    let engine = verified_engine(&identity);
    let mut plain = engine
        .tcp_connector()
        .execute(request(&peer))
        .expect("plain connect");
    assert_eq!(peer.event(), Event::Accepted);
    assert_eq!(
        read_plain_line(&mut plain),
        b"220 fixture.example ESMTP ready\r\n"
    );
    plain
        .send(b"STARTTLS\r\n".to_vec())
        .expect("STARTTLS command");
    assert_eq!(read_plain_line(&mut plain), b"220 Ready to start TLS\r\n");
    assert_eq!(peer.event(), Event::StartTlsReady);
    let original_local = plain.local_addr().expect("plain local address");
    let mut secure = plain
        .into_tls(options("127.0.0.1"))
        .expect("consuming TLS upgrade");
    assert_eq!(
        secure.local_addr().expect("TLS local address"),
        original_local
    );
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    secure
        .send(b"ping".to_vec())
        .expect("encrypted send after upgrade");
    read_tls_exact(&mut secure, b"secure");
    assert_eq!(secure.read(&mut [0_u8; 1]).expect("orderly TLS EOF"), None);
    assert_eq!(peer.event(), Event::Read(b"ping".to_vec()));
    assert_eq!(peer.event(), Event::Wrote);
    drop(secure);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn unknown_root_and_wrong_hostname_fail_after_peer_acceptance() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::Send {
            reply: Vec::new(),
            close: Close::Notify,
        }),
    );
    let engine = Engine::new(EngineConfig::spawned()).expect("ordinary Engine");
    let result = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"));
    assert_eq!(peer.event(), Event::Accepted);
    match result {
        Err(ExecuteError::Failed(error)) => assert_eq!(
            error.tls_failure(),
            Some(TlsFailure::CertificateUnknownIssuer)
        ),
        other => panic!("unknown root should fail certificate verification: {other:?}"),
    }
    drop(peer);
    engine.shutdown().expect("TLS Engine shutdown");

    let identity = TestIdentity::wrong_host();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::Send {
            reply: Vec::new(),
            close: Close::Notify,
        }),
    );
    let engine = verified_engine(&identity);
    let result = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"));
    assert_eq!(peer.event(), Event::Accepted);
    match result {
        Err(ExecuteError::Failed(error)) => assert_eq!(
            error.tls_failure(),
            Some(TlsFailure::CertificateHostnameMismatch)
        ),
        other => panic!("wrong hostname should fail certificate verification: {other:?}"),
    }
    drop(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn manual_engine_drives_tls_handshake_and_nonblocking_application_io() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::ReadThenSend {
            request_len: 1,
            reply: b"y".to_vec(),
            close: Close::Notify,
        }),
    );
    let mut engine = Engine::new(
        EngineConfig::manual().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("manual TLS Engine");
    let pending = engine
        .tcp_connector()
        .submit_tls(request(&peer), options("127.0.0.1"))
        .expect("manual TLS submit");
    let mut connection = match engine.drive_until(pending).expect("manual TLS drive") {
        TlsConnectCompletion::Completed(connection) => connection,
        other => panic!("manual handshake did not complete: {other:?}"),
    };
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    connection
        .try_send(b"x".to_vec())
        .expect("manual encrypted send");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut byte = [0_u8; 1];
    loop {
        assert!(Instant::now() < deadline, "manual TLS echo timed out");
        engine.drive(deadline).expect("manual TLS progress");
        match connection.try_read(&mut byte).expect("manual TLS read") {
            TcpRead::Pending => {}
            TcpRead::Data(1) => break,
            other => panic!("unexpected manual TLS read: {other:?}"),
        }
    }
    assert_eq!(byte, *b"y");
    assert_eq!(peer.event(), Event::Read(b"x".to_vec()));
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("manual TLS Engine shutdown");
}

#[test]
fn silent_peer_handshake_times_out_and_cancel_is_terminal() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let engine = verified_engine(&identity);
    let result = engine.tcp_connector().execute_tls(
        request(&peer),
        TlsOptions::new("127.0.0.1")
            .expect("TLS identity")
            .handshake_timeout(Duration::from_millis(150)),
    );
    assert_eq!(peer.event(), Event::Accepted);
    match result {
        Err(ExecuteError::Failed(error)) => assert_eq!(error.kind(), ErrorKind::Timeout),
        other => panic!("silent TLS peer should time out: {other:?}"),
    }
    drop(peer);
    engine.shutdown().expect("TLS Engine shutdown");

    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let engine = verified_engine(&identity);
    let pending = engine
        .tcp_connector()
        .submit_tls(request(&peer), options("127.0.0.1"))
        .expect("TLS submit before cancellation");
    assert_eq!(peer.event(), Event::Accepted);
    pending.handle().cancel().expect("cancel TLS handshake");
    match pending.wait_for(Duration::from_secs(3)) {
        TlsConnectWaitOutcome::Completed(TlsConnectCompletion::Cancelled) => {}
        other => panic!("TLS cancellation must commit promptly: {other:?}"),
    }
    drop(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn tls_reserve_is_admitted_atomically_and_reclaimed_after_connection_drop() {
    const PLAIN_WINDOWS: usize = 4096 + 4096;
    const TLS_RESERVE: usize = 256 * 1024;
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let config = EngineConfig::spawned()
        .with_additional_tls_root_certificate(identity.root_der.clone())
        .with_max_queued_bytes(PLAIN_WINDOWS + TLS_RESERVE - 1);
    let engine = Engine::new(config).expect("under-budget Engine");
    let error = engine
        .tcp_connector()
        .submit_tls(request(&peer), options("127.0.0.1"))
        .expect_err("one byte below the TLS reservation must reject before admission");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(error.limit_kind(), Some(LimitKind::TcpQueueBytes));
    assert_eq!(engine.metrics().tcp_connects_accepted(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    drop(peer);
    engine.shutdown().expect("under-budget Engine shutdown");

    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::Send {
            reply: b"x".to_vec(),
            close: Close::Notify,
        }),
    );
    let config = EngineConfig::spawned()
        .with_additional_tls_root_certificate(identity.root_der.clone())
        .with_max_queued_bytes(PLAIN_WINDOWS + TLS_RESERVE);
    let engine = Engine::new(config).expect("exact-budget Engine");
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"))
        .expect("exact budget admits verified TLS");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 1);
    assert_eq!(
        engine.metrics().current().reserved_tcp_queue_bytes(),
        PLAIN_WINDOWS + TLS_RESERVE
    );
    read_tls_exact(&mut connection, b"x");
    assert_eq!(
        connection.read(&mut [0_u8; 1]).expect("orderly TLS EOF"),
        None
    );
    drop(connection);
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    assert_eq!(peer.event(), Event::Wrote);
    assert_completed(peer);
    engine.shutdown().expect("exact-budget Engine shutdown");
}

#[test]
fn manual_blocking_tls_conveniences_reject_without_waiting_for_drive() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let mut engine = Engine::new(
        EngineConfig::manual().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("manual Engine");
    let connector = engine.tcp_connector();
    let tls_request = request(&peer);
    let (tx, rx) = mpsc::channel();
    let execute = thread::spawn(move || {
        tx.send(connector.execute_tls(tls_request, options("127.0.0.1")))
            .expect("send manual execute result");
    });
    let result = rx.recv_timeout(Duration::from_secs(1));
    if result.is_err() {
        engine.shutdown().expect("stop blocked manual execute");
        execute.join().expect("manual execute thread");
        panic!("manual execute_tls waited for drive instead of returning WrongMode");
    }
    match result.expect("manual execute result") {
        Err(ExecuteError::Submission(error)) => assert_eq!(error.kind(), ErrorKind::WrongMode),
        other => panic!("manual execute_tls should reject WrongMode: {other:?}"),
    }
    execute.join().expect("manual execute thread");
    drop(peer);
    engine.shutdown().expect("manual Engine shutdown");

    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let mut engine = Engine::new(
        EngineConfig::manual().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("manual Engine");
    let pending = engine
        .tcp_connector()
        .submit(request(&peer))
        .expect("plain submit");
    let plain = match engine.drive_until(pending).expect("manual plain drive") {
        nbreq::TcpConnectCompletion::Completed(connection) => connection,
        other => panic!("manual plain connect failed: {other:?}"),
    };
    assert_eq!(peer.event(), Event::Accepted);
    let (tx, rx) = mpsc::channel();
    let upgrade = thread::spawn(move || {
        tx.send(plain.into_tls(options("127.0.0.1")))
            .expect("send manual upgrade result");
    });
    let result = rx.recv_timeout(Duration::from_secs(1));
    if result.is_err() {
        engine.shutdown().expect("stop blocked manual upgrade");
        upgrade.join().expect("manual upgrade thread");
        panic!("manual into_tls waited for drive instead of returning WrongMode");
    }
    match result.expect("manual upgrade result") {
        Err(ExecuteError::Submission(error)) => assert_eq!(error.kind(), ErrorKind::WrongMode),
        other => panic!("manual into_tls should reject WrongMode: {other:?}"),
    }
    upgrade.join().expect("manual upgrade thread");
    drop(peer);
    engine.shutdown().expect("manual Engine shutdown");
}
