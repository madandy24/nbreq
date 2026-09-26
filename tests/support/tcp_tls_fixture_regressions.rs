//! Deterministic tests for TLS state already processed by the fixture's handshake pump.
use super::*;

const APPLICATION: &[u8] = b"split-write";

fn server_after_final_flight(close_notify: bool) -> ServerConnection {
    let identity = TestIdentity::localhost();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(identity.root_der))
        .expect("test root");
    let client_config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("client protocol")
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("server protocol")
        .with_no_client_auth()
        .with_single_cert(
            vec![identity.leaf_der],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der)),
        )
        .expect("server identity");
    let mut client = rustls::ClientConnection::new(
        Arc::new(client_config),
        rustls::pki_types::ServerName::try_from("127.0.0.1").expect("IP identity"),
    )
    .expect("client");
    let mut server = ServerConnection::new(Arc::new(server_config)).expect("server");

    let mut wire = Vec::new();
    while client.wants_write() {
        client.write_tls(&mut wire).expect("ClientHello");
    }
    server
        .read_tls(&mut Cursor::new(wire))
        .expect("server ClientHello input");
    server
        .process_new_packets()
        .expect("server ClientHello processing");
    let mut wire = Vec::new();
    while server.wants_write() {
        server.write_tls(&mut wire).expect("server flight");
    }
    client
        .read_tls(&mut Cursor::new(wire))
        .expect("client handshake input");
    client
        .process_new_packets()
        .expect("client verified handshake");
    assert!(!client.is_handshaking());
    assert!(server.is_handshaking());

    // The final client flight contains Finished followed by application data and,
    // optionally, close_notify. One process_new_packets call consumes the whole
    // flight just as the last handshake pump can, without relying on thread timing.
    client
        .writer()
        .write_all(APPLICATION)
        .expect("application write");
    if close_notify {
        client.send_close_notify();
    }
    let mut wire = Vec::new();
    while client.wants_write() {
        client.write_tls(&mut wire).expect("coalesced final flight");
    }
    assert_eq!(
        server
            .read_tls(&mut Cursor::new(&wire))
            .expect("final server input"),
        wire.len()
    );
    let state = server.process_new_packets().expect("server final flight");
    assert!(!server.is_handshaking());
    assert_eq!(state.peer_has_closed(), close_notify);
    assert_eq!(state.plaintext_bytes_to_read(), APPLICATION.len());
    server
}

fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("pair listener");
    listener.set_nonblocking(true).expect("bounded accept");
    let client = TcpStream::connect_timeout(
        &listener.local_addr().expect("pair address"),
        Duration::from_secs(1),
    )
    .expect("pair connect");
    let deadline = Instant::now() + Duration::from_secs(1);
    let server = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if retry(&error) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) => panic!("bounded pair accept: {error}"),
        }
    };
    server
        .set_nonblocking(false)
        .expect("blocking accepted socket");
    server
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("bounded read");
    server
        .set_write_timeout(Some(Duration::from_millis(100)))
        .expect("bounded write");
    (client, server)
}

#[test]
fn authenticated_close_in_final_handshake_flight_preserves_application_data() {
    let mut tls = server_after_final_flight(true);
    let (_peer, mut socket) = socket_pair();
    let (events, receiver) = mpsc::channel();
    let observed = observe_client_close(
        &mut tls,
        &mut socket,
        &events,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(1),
    )
    .expect("close observer");
    assert!(
        observed,
        "fixture must recognize the authenticated close already processed with the final handshake flight"
    );
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("close event"),
        Event::PeerCloseNotify {
            application: APPLICATION.to_vec()
        }
    );
    assert!(
        matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "one authenticated close event"
    );
}

#[test]
fn raw_eof_after_final_handshake_flight_is_not_authenticated_close() {
    let mut tls = server_after_final_flight(false);
    let (peer, mut socket) = socket_pair();
    peer.shutdown(std::net::Shutdown::Write)
        .expect("raw transport EOF");
    let (events, receiver) = mpsc::channel();
    let observed = observe_client_close(
        &mut tls,
        &mut socket,
        &events,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(1),
    )
    .expect("close observer");
    assert!(
        !observed,
        "raw EOF must never become an authenticated TLS close"
    );
    assert!(
        matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "raw EOF must not emit a close_notify event"
    );
}
