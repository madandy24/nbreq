#[cfg(feature = "native")]
use std::time::Instant;

use nbreq::EngineBuilder;
use nbreq::{
    Client, DetachedCallbacks, Engine, EngineConfig, EngineMetrics, ErrorKind, HttpBackend,
    PendingRequest, PendingTcpConnect, RequestHandle, ResourceMetrics, ResponseReader,
    StreamRequest, TcpConnection, TcpConnector, TcpFinishStatus, TcpReader, TcpWriter, UploadBody,
    UploadSender,
};
#[cfg(feature = "resolver")]
use nbreq::{PendingResolve, Resolver};

fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn public_thread_traits_match_the_contract() {
    assert_send::<Engine>();
    assert_send_sync::<Client>();
    assert_send_sync::<RequestHandle>();
    assert_send::<PendingRequest>();
    assert_send::<DetachedCallbacks>();
    assert_send::<StreamRequest>();
    assert_send::<UploadBody>();
    assert_send::<UploadSender>();
    assert_send::<ResponseReader>();
    assert_send_sync::<EngineMetrics>();
    assert_send_sync::<ResourceMetrics>();
    #[cfg(feature = "resolver")]
    assert_send_sync::<Resolver>();
    assert_send_sync::<TcpConnector>();
    #[cfg(feature = "resolver")]
    assert_send::<PendingResolve>();
    assert_send::<PendingTcpConnect>();
    assert_send::<TcpConnection>();
    assert_send::<TcpReader>();
    assert_send::<TcpWriter>();
    assert_send::<TcpFinishStatus>();
}

#[test]
#[cfg(feature = "native")]
fn clients_are_engine_issued_and_do_not_own_shutdown() {
    let engine = Engine::new(EngineConfig::spawned()).expect("Engine must construct");
    let client = engine.client();
    let clone = client.clone();
    drop(client);
    drop(clone);
    engine.shutdown().expect("empty Engine must stop");
}

#[test]
#[cfg(feature = "native")]
fn metrics_are_owner_observed_and_start_empty() {
    let engine = Engine::new(EngineConfig::spawned()).expect("Engine must construct");
    let metrics = engine.metrics();
    assert!(metrics.connection_metrics_available());
    assert_eq!(metrics.current(), ResourceMetrics::default());
    engine.shutdown().expect("empty Engine must stop");
}

#[test]
#[cfg(feature = "native")]
fn ordinary_and_explicit_native_selection_are_available() {
    let ordinary = Engine::new(EngineConfig::spawned())
        .expect("ordinary construction must use compiled native");
    assert!(ordinary.metrics().connection_metrics_available());
    ordinary.shutdown().expect("ordinary Engine must stop");

    let engine = Engine::builder()
        .http_backend(HttpBackend::Native)
        .build()
        .expect("compiled native backend must construct explicitly");
    assert!(engine.metrics().connection_metrics_available());
    engine.shutdown().expect("empty native Engine must stop");

    let manual = EngineBuilder::manual()
        .http_backend(HttpBackend::Native)
        .build()
        .expect("compiled native backend must support explicit manual construction");
    assert!(manual.metrics().connection_metrics_available());
    manual
        .shutdown()
        .expect("empty manual native Engine must stop");
}

#[test]
#[cfg(not(feature = "native"))]
fn ordinary_construction_fails_without_the_native_feature() {
    let spawned = Engine::new(EngineConfig::spawned())
        .err()
        .expect("ordinary spawned construction must fail without native");
    assert_eq!(spawned.kind(), ErrorKind::Unsupported);

    let built = Engine::builder()
        .build()
        .err()
        .expect("unqualified builder construction must fail without native");
    assert_eq!(built.kind(), ErrorKind::Unsupported);

    let manual = EngineBuilder::manual()
        .build()
        .err()
        .expect("ordinary manual construction must fail without native");
    assert_eq!(manual.kind(), ErrorKind::Unsupported);
}

#[test]
#[cfg(not(feature = "native"))]
fn explicit_native_selection_fails_when_unavailable() {
    let error = Engine::builder()
        .http_backend(HttpBackend::Native)
        .build()
        .err()
        .expect("uncompiled native backend must fail construction");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}

#[test]
#[cfg(feature = "native")]
fn spawned_drive_fails_explicitly() {
    let mut engine = Engine::new(EngineConfig::spawned()).expect("Engine must construct");
    let error = engine
        .drive(Instant::now())
        .expect_err("spawned Engine must reject manual drive");
    assert_eq!(error.kind(), ErrorKind::WrongMode);
}

#[test]
#[cfg(feature = "native")]
fn manual_drive_uses_the_same_public_engine_type() {
    let mut engine = Engine::new(EngineConfig::manual()).expect("native Engine must construct");
    let _status = engine
        .drive(Instant::now())
        .expect("manual native drive must succeed");
    engine.shutdown().expect("empty Engine must stop");
}

fn drive_http(
    engine: &mut Engine,
    pending: PendingRequest,
) -> Result<nbreq::Completion, nbreq::Error> {
    engine.drive_until(pending)
}

#[cfg(feature = "resolver")]
fn drive_resolve(
    engine: &mut Engine,
    pending: PendingResolve,
) -> Result<nbreq::ResolveCompletion, nbreq::Error> {
    engine.drive_until(pending)
}

fn drive_connect(
    engine: &mut Engine,
    pending: PendingTcpConnect,
) -> Result<nbreq::TcpConnectCompletion, nbreq::Error> {
    engine.drive_until(pending)
}

fn drive_with_public_bound<T: nbreq::WaiterTarget>(
    engine: &mut Engine,
    pending: T,
) -> Result<T::Output, nbreq::Error> {
    engine.drive_until(pending)
}

#[test]
fn generic_drive_until_returns_each_terminal_type() {
    let _http: fn(&mut Engine, PendingRequest) -> Result<nbreq::Completion, nbreq::Error> =
        drive_http;
    #[cfg(feature = "resolver")]
    let _dns: fn(
        &mut Engine,
        PendingResolve,
    ) -> Result<nbreq::ResolveCompletion, nbreq::Error> = drive_resolve;
    let _tcp: fn(
        &mut Engine,
        PendingTcpConnect,
    ) -> Result<nbreq::TcpConnectCompletion, nbreq::Error> = drive_connect;
}

#[test]
fn generic_drive_until_accepts_exactly_the_public_waiter_bound() {
    let _http: fn(&mut Engine, PendingRequest) -> Result<nbreq::Completion, nbreq::Error> =
        drive_with_public_bound;
    #[cfg(feature = "resolver")]
    let _dns: fn(
        &mut Engine,
        PendingResolve,
    ) -> Result<nbreq::ResolveCompletion, nbreq::Error> = drive_with_public_bound;
    let _tcp: fn(
        &mut Engine,
        PendingTcpConnect,
    ) -> Result<nbreq::TcpConnectCompletion, nbreq::Error> = drive_with_public_bound;
}

#[test]
#[cfg(all(feature = "native", feature = "resolver"))]
fn resolver_and_tcp_tickets_share_the_native_engine() {
    let engine = Engine::new(EngineConfig::spawned()).expect("Engine must construct");
    let resolver = engine.resolver();
    let tcp = engine.tcp_connector();
    let clone = resolver.clone();
    drop(clone);
    let before = engine.metrics();
    assert_eq!(before.resolutions_accepted(), 0);
    assert_eq!(before.tcp_connects_accepted(), 0);
    assert_eq!(before.current().inflight_resolutions(), 0);
    assert_eq!(before.current().standalone_tcp_connections(), 0);

    let connect = nbreq::TcpConnectRequest::hostname("example.com", 9)
        .build()
        .expect("connect request must build");
    assert!(matches!(
        connect.target(),
        nbreq::TcpConnectTarget::Hostname { name, port }
            if name == "example.com" && *port == 9
    ));
    drop(tcp);

    let after = engine.metrics();
    assert_eq!(before, after);
    engine.shutdown().expect("empty Engine must stop");
}

#[test]
#[cfg(feature = "native")]
fn literal_tcp_connects_through_the_ordinary_native_engine() {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::thread;
    use std::time::Duration;

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind public TCP fixture");
    let address = listener.local_addr().expect("public TCP fixture address");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept public TCP connection");
        let mut request = [0_u8; 4];
        socket.read_exact(&mut request).expect("read public bytes");
        assert_eq!(&request, b"ping");
        socket.write_all(b"pong").expect("write public bytes");
    });

    let engine =
        Engine::new(EngineConfig::spawned()).expect("ordinary native Engine must construct");
    let request = nbreq::TcpConnectRequest::literal(address)
        .connect_timeout(Duration::from_secs(2))
        .build()
        .expect("literal public request must build");
    let mut connection = engine
        .tcp_connector()
        .execute(request)
        .expect("ordinary native Engine must connect literal TCP");
    connection
        .send(b"ping".to_vec())
        .expect("send public bytes");
    let mut response = [0_u8; 4];
    assert_eq!(
        connection.read(&mut response).expect("read public bytes"),
        Some(4)
    );
    assert_eq!(&response, b"pong");
    drop(connection);
    server.join().expect("public TCP fixture must join");
    engine.shutdown().expect("ordinary native Engine must stop");
}
