#![cfg(feature = "native")]

#[path = "support/tcp_tls_fixture.rs"]
mod fixture;

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};
use std::{sync::mpsc, thread};

use fixture::{
    Close, Entry, Event, Exchange, Outcome, PeerOptions, Phase, Protocol, StartTlsResponse,
    TestIdentity, TestPeer,
};
use nbreq::{
    Engine, EngineConfig, ErrorKind, ExecuteError, LimitKind, TcpConnectRequest, TcpFinishStatus,
    TcpRead, TcpSendErrorKind, TcpStreamError, TlsConnectCompletion, TlsConnectWaitOutcome,
    TlsConnection, TlsFailure, TlsOptions, TransportStage,
};

fn request(peer: &TestPeer) -> nbreq::TcpConnectRequest {
    TcpConnectRequest::literal(peer.address)
        .connect_timeout(Duration::from_secs(2))
        .read_inactivity_timeout(Duration::from_secs(6))
        .write_inactivity_timeout(Duration::from_secs(6))
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
        .handshake_timeout(Duration::from_secs(6))
}

fn read_tls_exact(connection: &mut TlsConnection, expected: &[u8]) {
    let (disarm, deadline) = mpsc::channel();
    let cancel = connection.handle();
    let watchdog = thread::spawn(move || {
        if matches!(
            deadline.recv_timeout(Duration::from_secs(8)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            let _ = cancel.cancel();
        }
    });
    let mut actual = vec![0_u8; expected.len()];
    let mut used = 0;
    while used < actual.len() {
        let read = connection.read(&mut actual[used..]).expect("TLS read");
        let count = read.expect("TLS peer closed before expected bytes");
        assert!(count > 0, "TLS read made no progress");
        used += count;
    }
    assert_eq!(actual, expected);
    disarm.send(()).expect("disarm TLS read watchdog");
    watchdog.join().expect("TLS read watchdog joins");
}

fn read_tls_exact_bounded(connection: &mut TlsConnection, expected: &[u8], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut actual = vec![0_u8; expected.len()];
    let mut used = 0;
    while used < actual.len() {
        assert!(Instant::now() < deadline, "TLS read exceeded test deadline");
        match connection
            .try_read(&mut actual[used..])
            .expect("TLS nonblocking read")
        {
            TcpRead::Pending => thread::sleep(Duration::from_millis(1)),
            TcpRead::Data(count) if count > 0 => used += count,
            other => panic!("TLS peer closed or made no progress before expected bytes: {other:?}"),
        }
    }
    assert_eq!(actual, expected);
}

fn read_tls_eof_bounded(connection: &mut TlsConnection, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        assert!(Instant::now() < deadline, "TLS EOF exceeded test deadline");
        match connection
            .try_read(&mut [0_u8; 1])
            .expect("TLS nonblocking EOF read")
        {
            TcpRead::Pending => thread::sleep(Duration::from_millis(1)),
            TcpRead::Eof => return,
            other => panic!("unexpected data before TLS EOF: {other:?}"),
        }
    }
}

fn finish_tls_bounded(connection: &mut TlsConnection, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        assert!(
            Instant::now() < deadline,
            "TLS finish exceeded test deadline"
        );
        match connection.try_finish().expect("TLS finish") {
            TcpFinishStatus::Pending => thread::sleep(Duration::from_millis(1)),
            TcpFinishStatus::Finished => return,
            other => panic!("unexpected TLS finish status: {other:?}"),
        }
    }
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

fn read_plain_exact(connection: &mut nbreq::TcpConnection, expected: &[u8]) {
    let mut actual = vec![0_u8; expected.len()];
    let mut used = 0;
    while used < actual.len() {
        let count = connection
            .read(&mut actual[used..])
            .expect("plain protocol read")
            .expect("plain peer closed before expected bytes");
        assert!(count > 0);
        used += count;
    }
    assert_eq!(actual, expected);
}

fn assert_completed(peer: TestPeer) {
    match peer.join() {
        Outcome::Completed => {}
        Outcome::Failed(message) => panic!("fixture exchange failed: {message}"),
        other => panic!("fixture exchange did not complete: {other:?}"),
    }
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
    read_tls_eof_bounded(&mut connection, Duration::from_secs(8));
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
    read_tls_eof_bounded(&mut connection, Duration::from_secs(8));
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
    let engine = Engine::new(
        EngineConfig::spawned()
            .with_additional_tls_root_certificate(identity.root_der.clone())
            .with_max_standalone_tcp_connections(NonZeroUsize::new(1).expect("one slot"))
            .with_max_queued_bytes(4096 + 4096 + 256 * 1024),
    )
    .expect("one-slot exact-budget Engine");
    let mut plain = engine
        .tcp_connector()
        .execute(request(&peer))
        .expect("plain connect");
    let original_id = plain.handle().id();
    assert_eq!(engine.metrics().tcp_connects_accepted(), 1);
    assert_eq!(engine.metrics().tcp_connects_completed(), 1);
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 1);
    assert_eq!(
        engine.metrics().current().reserved_tcp_queue_bytes(),
        4096 + 4096
    );
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
    assert_eq!(secure.handle().id(), original_id);
    assert_eq!(engine.metrics().tcp_connects_accepted(), 1);
    assert_eq!(engine.metrics().tcp_connects_completed(), 1);
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 1);
    assert_eq!(
        engine.metrics().current().reserved_tcp_queue_bytes(),
        4096 + 4096 + 256 * 1024
    );
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    secure
        .send(b"ping".to_vec())
        .expect("encrypted send after upgrade");
    read_tls_exact(&mut secure, b"secure");
    read_tls_eof_bounded(&mut secure, Duration::from_secs(8));
    assert_eq!(peer.event(), Event::Read(b"ping".to_vec()));
    assert_eq!(peer.event(), Event::Wrote);
    drop(secure);
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn starttls_waits_for_a_fragmented_ready_line_before_upgrading() {
    let identity = TestIdentity::localhost();
    let mut peer_options = PeerOptions::new(Exchange::Send {
        reply: b"secure".to_vec(),
        close: Close::Notify,
    });
    peer_options.entry = Entry::StartTls;
    peer_options.starttls_response = StartTlsResponse::Fragmented;
    peer_options.holds.insert(Phase::BeforeStartTlsAckTail);
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
    peer.wait_held(Phase::BeforeStartTlsAckTail);
    read_plain_exact(&mut plain, b"220 Ready");
    peer.release(Phase::BeforeStartTlsAckTail);
    assert_eq!(read_plain_line(&mut plain), b" to start TLS\r\n");
    assert_eq!(peer.event(), Event::StartTlsReady);
    let mut secure = plain
        .into_tls(options("127.0.0.1"))
        .expect("upgrade after full 220 line");
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    read_tls_exact(&mut secure, b"secure");
    read_tls_eof_bounded(&mut secure, Duration::from_secs(8));
    assert_eq!(peer.event(), Event::Wrote);
    drop(secure);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn starttls_rejects_plaintext_trailing_the_upgrade_reply() {
    let identity = TestIdentity::localhost();
    let mut peer_options = PeerOptions::new(Exchange::SilentHandshake);
    peer_options.entry = Entry::StartTls;
    peer_options.starttls_response = StartTlsResponse::Trailing(vec![b'X'; 32]);
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
    let pending = plain.submit_tls(options("127.0.0.1"));
    match pending {
        Err(error) => assert_eq!(error.kind(), ErrorKind::InvalidRequest),
        Ok(pending) => match pending.wait_for(Duration::from_secs(8)) {
            TlsConnectWaitOutcome::Completed(TlsConnectCompletion::Failed(_)) => {}
            other => panic!("trailing plaintext must not become a TLS session: {other:?}"),
        },
    }
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    drop(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn zero_handshake_timeout_rejects_immediate_admission_and_consumed_upgrade() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let engine = verified_engine(&identity);
    let zero = TlsOptions::new("127.0.0.1")
        .expect("TLS identity")
        .handshake_timeout(Duration::ZERO);
    let error = engine
        .tcp_connector()
        .submit_tls(request(&peer), zero)
        .expect_err("zero handshake timeout must reject before admission");
    assert_eq!(error.kind(), ErrorKind::InvalidRequest);
    assert_eq!(engine.metrics().tcp_connects_accepted(), 0);
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    drop(peer);
    engine.shutdown().expect("TLS Engine shutdown");

    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let engine = verified_engine(&identity);
    let plain = engine
        .tcp_connector()
        .execute(request(&peer))
        .expect("plain connect");
    assert_eq!(peer.event(), Event::Accepted);
    let zero = TlsOptions::new("127.0.0.1")
        .expect("TLS identity")
        .handshake_timeout(Duration::ZERO);
    let error = plain
        .submit_tls(zero)
        .expect_err("zero-timeout upgrade consumes and aborts plain socket");
    assert_eq!(error.kind(), ErrorKind::InvalidRequest);
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    drop(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn upgrade_rejects_queued_plaintext_output_before_tls_admission() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let mut engine = Engine::new(
        EngineConfig::manual().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("manual Engine");
    let pending = engine
        .tcp_connector()
        .submit(request(&peer))
        .expect("plain submit");
    let mut plain = match engine.drive_until(pending).expect("plain connect drive") {
        nbreq::TcpConnectCompletion::Completed(connection) => connection,
        other => panic!("plain connection did not complete: {other:?}"),
    };
    assert_eq!(peer.event(), Event::Accepted);
    plain
        .try_send(b"still-plain".to_vec())
        .expect("queue cleartext before upgrade");
    let error = plain
        .submit_tls(options("127.0.0.1"))
        .expect_err("queued cleartext must make the upgrade boundary dirty");
    assert_eq!(error.kind(), ErrorKind::InvalidRequest);
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    engine
        .drive(Instant::now())
        .expect("reap consumed dirty connection");
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    drop(peer);
    engine.shutdown().expect("manual Engine shutdown");
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
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
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
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
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
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
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
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
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
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    drop(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn stalled_tls_handshake_does_not_block_another_connection() {
    let identity = TestIdentity::localhost();
    let stalled = TestPeer::spawn(&identity, PeerOptions::new(Exchange::SilentHandshake));
    let healthy = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::Send {
            reply: b"healthy".to_vec(),
            close: Close::Notify,
        }),
    );
    let engine = verified_engine(&identity);
    let pending = engine
        .tcp_connector()
        .submit_tls(request(&stalled), options("127.0.0.1"))
        .expect("submit stalled handshake");
    assert_eq!(stalled.event(), Event::Accepted);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&healthy), options("127.0.0.1"))
        .expect("healthy TLS session must complete beside stalled peer");
    assert_eq!(healthy.event(), Event::Accepted);
    read_tls_exact_bounded(&mut connection, b"healthy", Duration::from_secs(8));
    read_tls_eof_bounded(&mut connection, Duration::from_secs(8));
    assert!(matches!(healthy.event(), Event::HandshakeComplete { .. }));
    assert_eq!(healthy.event(), Event::Wrote);
    drop(connection);
    assert_completed(healthy);
    pending.handle().cancel().expect("cancel stalled handshake");
    match pending.wait_for(Duration::from_secs(3)) {
        TlsConnectWaitOutcome::Completed(TlsConnectCompletion::Cancelled) => {}
        other => panic!("stalled handshake cancellation must complete: {other:?}"),
    }
    drop(stalled);
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
    read_tls_eof_bounded(&mut connection, Duration::from_secs(8));
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
    let engine = Engine::new(
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
    assert_eq!(engine.metrics().tcp_connects_accepted(), 0);
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
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
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
    engine
        .drive(Instant::now())
        .expect("reap consumed WrongMode connection");
    upgrade.join().expect("manual upgrade thread");
    drop(peer);
    engine.shutdown().expect("manual Engine shutdown");
}

#[test]
fn idle_tls13_key_update_sends_its_control_reply_without_application_output() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::KeyUpdateIdle));
    let engine = verified_engine(&identity);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"))
        .expect("TLS 1.3 connection");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(
        peer.event(),
        Event::HandshakeComplete {
            version: rustls::ProtocolVersion::TLSv1_3,
            ..
        }
    ));
    // No application bytes are queued by the client. Reading the marker must also flush the
    // KeyUpdate response generated while processing incoming control traffic.
    read_tls_exact_bounded(&mut connection, b"updated", Duration::from_secs(8));
    assert_eq!(peer.event(), Event::PostUpdateControlFlight);
    assert_eq!(peer.event(), Event::Wrote);
    read_tls_eof_bounded(&mut connection, Duration::from_secs(8));
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn one_byte_receive_window_preserves_more_than_64k_of_tls_plaintext() {
    let identity = TestIdentity::localhost();
    let expected: Vec<u8> = (0..65_537).map(|index| (index % 251) as u8).collect();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::Send {
            reply: expected.clone(),
            close: Close::Notify,
        }),
    );
    let mut engine = Engine::new(
        EngineConfig::manual().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("manual tiny-window Engine");
    let tiny_request = TcpConnectRequest::literal(peer.address)
        .connect_timeout(Duration::from_secs(2))
        .read_inactivity_timeout(Duration::from_secs(6))
        .write_inactivity_timeout(Duration::from_secs(6))
        .send_queue_bytes(1)
        .receive_queue_bytes(1)
        .build()
        .expect("tiny-window TLS request");
    let pending = engine
        .tcp_connector()
        .submit_tls(tiny_request, options("127.0.0.1"))
        .expect("tiny-window TLS submit");
    let mut connection = match engine.drive_until(pending).expect("tiny-window TLS drive") {
        TlsConnectCompletion::Completed(connection) => connection,
        other => panic!("tiny-window TLS connection failed: {other:?}"),
    };
    assert_eq!(peer.event(), Event::Accepted);
    let deadline = Instant::now() + Duration::from_secs(15);
    for expected_byte in expected {
        let mut byte = [0_u8; 1];
        loop {
            assert!(
                Instant::now() < deadline,
                "large one-byte TLS read exceeded test deadline"
            );
            match connection.try_read(&mut byte).expect("one-byte TLS read") {
                TcpRead::Pending => {
                    engine
                        .drive(Instant::now())
                        .expect("manual tiny-window progress");
                }
                TcpRead::Data(1) => break,
                other => panic!("one-byte TLS read ended early: {other:?}"),
            }
        }
        assert_eq!(byte, [expected_byte]);
    }
    let eof_deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            Instant::now() < eof_deadline,
            "manual TLS EOF exceeded test deadline"
        );
        match connection
            .try_read(&mut [0_u8; 1])
            .expect("manual TLS EOF read")
        {
            TcpRead::Pending => {
                engine
                    .drive(Instant::now())
                    .expect("manual TLS EOF progress");
            }
            TcpRead::Eof => break,
            other => panic!("unexpected data before manual TLS EOF: {other:?}"),
        }
    }
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn tls13_local_finish_drains_output_and_preserves_the_reader() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::ObserveClientFinishThenSend {
            reply: b"after".to_vec(),
        }),
    );
    let engine = verified_engine(&identity);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"))
        .expect("TLS 1.3 connection");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(
        peer.event(),
        Event::HandshakeComplete {
            version: rustls::ProtocolVersion::TLSv1_3,
            ..
        }
    ));
    connection
        .send(b"drain".to_vec())
        .expect("accepted TLS output");
    finish_tls_bounded(&mut connection, Duration::from_secs(8));
    assert_eq!(
        peer.event(),
        Event::PeerCloseNotify {
            application: b"drain".to_vec()
        }
    );
    read_tls_exact_bounded(&mut connection, b"after", Duration::from_secs(8));
    read_tls_eof_bounded(&mut connection, Duration::from_secs(8));
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn tls12_peer_close_gets_a_close_response_and_closes_the_writer() {
    let identity = TestIdentity::localhost();
    let mut peer_options = PeerOptions::new(Exchange::PeerCloseThenObserve);
    peer_options.protocol = Protocol::Tls12;
    let peer = TestPeer::spawn(&identity, peer_options);
    let engine = verified_engine(&identity);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"))
        .expect("TLS 1.2 connection");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(
        peer.event(),
        Event::HandshakeComplete {
            version: rustls::ProtocolVersion::TLSv1_2,
            ..
        }
    ));
    read_tls_eof_bounded(&mut connection, Duration::from_secs(8));
    assert_eq!(peer.event(), Event::Wrote);
    assert_eq!(
        peer.event(),
        Event::PeerCloseNotify {
            application: Vec::new()
        }
    );
    let error = connection
        .try_send(b"late".to_vec())
        .expect_err("TLS 1.2 peer close ends sending");
    assert_eq!(error.kind(), TcpSendErrorKind::Closed);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn tls13_peer_close_pauses_read_timeout_while_writer_remains_open() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(&identity, PeerOptions::new(Exchange::PeerCloseThenObserve));
    let engine = verified_engine(&identity);
    let request = TcpConnectRequest::literal(peer.address)
        .connect_timeout(Duration::from_secs(2))
        .read_inactivity_timeout(Duration::from_millis(100))
        .write_inactivity_timeout(Duration::from_secs(6))
        .send_queue_bytes(4096)
        .receive_queue_bytes(4096)
        .build()
        .expect("short-read-timeout request");
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request, options("127.0.0.1"))
        .expect("TLS 1.3 connection");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(
        peer.event(),
        Event::HandshakeComplete {
            version: rustls::ProtocolVersion::TLSv1_3,
            ..
        }
    ));
    read_tls_eof_bounded(&mut connection, Duration::from_secs(8));
    assert_eq!(peer.event(), Event::Wrote);
    thread::sleep(Duration::from_millis(250));
    connection
        .send(b"still-writing".to_vec())
        .expect("TLS 1.3 writer remains open beyond read inactivity deadline");
    finish_tls_bounded(&mut connection, Duration::from_secs(8));
    assert_eq!(
        peer.event(),
        Event::PeerCloseNotify {
            application: b"still-writing".to_vec()
        }
    );
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn incomplete_encrypted_record_is_a_tls_failure_not_orderly_eof() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::Send {
            reply: b"secret".to_vec(),
            close: Close::RawMidRecord,
        }),
    );
    let engine = verified_engine(&identity);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"))
        .expect("TLS connection before truncated record");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            Instant::now() < deadline,
            "truncated TLS record exceeded test deadline"
        );
        match connection.try_read(&mut [0_u8; 16]) {
            Ok(TcpRead::Pending) => thread::sleep(Duration::from_millis(1)),
            Err(TcpStreamError::Failed(error)) => {
                assert_eq!(error.tls_failure(), Some(TlsFailure::Truncated));
                assert_eq!(error.transport_stage(), Some(TransportStage::Receive));
                break;
            }
            other => {
                panic!("partial TLS record must fail instead of yielding data or EOF: {other:?}")
            }
        }
    }
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn raw_tcp_close_after_complete_tls_record_is_not_orderly_eof() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::Send {
            reply: b"complete".to_vec(),
            close: Close::Raw,
        }),
    );
    let engine = verified_engine(&identity);
    let mut connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options("127.0.0.1"))
        .expect("TLS connection before raw TCP close");
    assert_eq!(peer.event(), Event::Accepted);
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut delivered = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "raw-close TLS read exceeded deadline"
        );
        let mut byte = [0_u8; 1];
        match connection.try_read(&mut byte) {
            Ok(TcpRead::Pending) => thread::sleep(Duration::from_millis(1)),
            Ok(TcpRead::Data(1)) => {
                delivered.push(byte[0]);
                assert!(b"complete".starts_with(&delivered));
            }
            Err(TcpStreamError::Failed(error)) => {
                assert_eq!(error.tls_failure(), Some(TlsFailure::Truncated));
                assert_eq!(error.transport_stage(), Some(TransportStage::Receive));
                break;
            }
            other => panic!("raw TCP close must not look like TLS close_notify: {other:?}"),
        }
    }
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}
