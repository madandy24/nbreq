#![cfg(feature = "native")]

#[allow(dead_code)]
#[path = "support/tcp_tls_fixture.rs"]
mod fixture;

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use fixture::{Close, Event, Exchange, Outcome, PeerOptions, Phase, TestIdentity, TestPeer};
use nbreq::{
    Engine, EngineConfig, TcpConnectRequest, TcpRead, TlsConnectCompletion, TlsConnectWaitOutcome,
    TlsOptions,
};

fn engine(identity: &TestIdentity) -> Engine {
    Engine::new(
        EngineConfig::spawned().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("verified TLS Engine")
}

fn request(peer: &TestPeer) -> TcpConnectRequest {
    TcpConnectRequest::literal(peer.address)
        .connect_timeout(Duration::from_secs(2))
        .build()
        .expect("loopback TLS request")
}

fn options() -> TlsOptions {
    TlsOptions::new("127.0.0.1")
        .expect("certificate IP identity")
        .handshake_timeout(Duration::from_secs(6))
}

fn read_exact_bounded(
    mut read: impl FnMut(&mut [u8]) -> Result<TcpRead, nbreq::TcpStreamError>,
    expected: &[u8],
) {
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut actual = vec![0; expected.len()];
    let mut filled = 0;
    while filled < actual.len() {
        assert!(
            Instant::now() < deadline,
            "TLS read exceeded fixture deadline"
        );
        match read(&mut actual[filled..]).expect("verified TLS read") {
            TcpRead::Pending => thread::sleep(Duration::from_millis(1)),
            TcpRead::Data(count) if count > 0 => filled += count,
            other => panic!("TLS read ended before expected plaintext: {other:?}"),
        }
    }
    assert_eq!(actual, expected);
}

fn assert_completed(peer: TestPeer) {
    match peer.join() {
        Outcome::Completed => {}
        other => panic!("TLS fixture did not complete: {other:?}"),
    }
}

#[test]
fn tls_callback_dispatch_delivers_verified_connection_once() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::Send {
            reply: b"callback".to_vec(),
            close: Close::Notify,
        }),
    );
    let engine = engine(&identity);
    let (tx, rx) = mpsc::channel();
    let handle = engine
        .tcp_connector()
        .start_tls(request(&peer), options(), move |completion| {
            tx.send(completion).expect("single TLS callback receiver");
        })
        .expect("callback TLS admission");
    let mut connection = match rx
        .recv_timeout(Duration::from_secs(6))
        .expect("TLS callback")
    {
        TlsConnectCompletion::Completed(connection) => connection,
        other => panic!("callback must carry a verified connection: {other:?}"),
    };
    assert_eq!(connection.handle().id(), handle.id());
    read_exact_bounded(|bytes| connection.try_read(bytes), b"callback");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn tls_wait_timeout_returns_the_same_live_pending_operation() {
    let identity = TestIdentity::localhost();
    let mut peer_options = PeerOptions::new(Exchange::Send {
        reply: b"resumed".to_vec(),
        close: Close::Notify,
    });
    peer_options.holds.insert(Phase::Handshake);
    let peer = TestPeer::spawn(&identity, peer_options);
    let engine = engine(&identity);
    let pending = engine
        .tcp_connector()
        .submit_tls(request(&peer), options())
        .expect("pending TLS admission");
    let original_id = pending.handle().id();
    assert_eq!(peer.event(), Event::Accepted);
    peer.wait_held(Phase::Handshake);
    let pending = match pending.wait_for(Duration::from_millis(50)) {
        TlsConnectWaitOutcome::TimedOut(pending) => pending,
        other => panic!("local wait must expire without cancelling TLS: {other:?}"),
    };
    assert_eq!(pending.handle().id(), original_id);
    assert!(!pending.is_complete());
    peer.release(Phase::Handshake);
    let mut connection = match pending.wait_for(Duration::from_secs(6)) {
        TlsConnectWaitOutcome::Completed(TlsConnectCompletion::Completed(connection)) => connection,
        other => panic!("same pending TLS operation must complete: {other:?}"),
    };
    assert_eq!(connection.handle().id(), original_id);
    read_exact_bounded(|bytes| connection.try_read(bytes), b"resumed");
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    assert_eq!(peer.event(), Event::Wrote);
    drop(connection);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}

#[test]
fn split_tls_reader_remains_usable_after_writer_finishes() {
    let identity = TestIdentity::localhost();
    let peer = TestPeer::spawn(
        &identity,
        PeerOptions::new(Exchange::ObserveClientFinishThenSend {
            reply: b"after-close".to_vec(),
        }),
    );
    let engine = engine(&identity);
    let connection = engine
        .tcp_connector()
        .execute_tls(request(&peer), options())
        .expect("verified TLS connection");
    let id = connection.handle().id();
    let (mut reader, mut writer) = connection.split();
    assert_eq!(reader.handle().id(), id);
    assert_eq!(writer.handle().id(), id);
    writer
        .send(b"split-write".to_vec())
        .expect("split TLS send");
    writer.finish().expect("authenticated TLS write close");
    read_exact_bounded(|bytes| reader.try_read(bytes), b"after-close");
    assert_eq!(peer.event(), Event::Accepted);
    assert!(matches!(peer.event(), Event::HandshakeComplete { .. }));
    assert_eq!(
        peer.event(),
        Event::PeerCloseNotify {
            application: b"split-write".to_vec(),
        }
    );
    assert_eq!(peer.event(), Event::Wrote);
    drop(reader);
    drop(writer);
    assert_completed(peer);
    engine.shutdown().expect("TLS Engine shutdown");
}
