#[cfg(feature = "native")]
use std::time::Instant;

use nbreq::EngineBuilder;
use nbreq::{
    Client, DetachedCallbacks, Engine, EngineConfig, EngineMetrics, ErrorKind, HttpBackend,
    PendingRequest, PendingResolve, PendingTcpConnect, RequestHandle, Resolver, ResourceMetrics,
    ResponseReader, StreamRequest, TcpConnection, TcpConnector, TcpFinishStatus, TcpReader,
    TcpWriter, UploadBody, UploadSender,
};

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
    assert_send_sync::<Resolver>();
    assert_send_sync::<TcpConnector>();
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
    let _dns: fn(&mut Engine, PendingResolve) -> Result<nbreq::ResolveCompletion, nbreq::Error> =
        drive_resolve;
    let _tcp: fn(
        &mut Engine,
        PendingTcpConnect,
    ) -> Result<nbreq::TcpConnectCompletion, nbreq::Error> = drive_connect;
}

#[test]
fn generic_drive_until_accepts_exactly_the_public_waiter_bound() {
    let _http: fn(&mut Engine, PendingRequest) -> Result<nbreq::Completion, nbreq::Error> =
        drive_with_public_bound;
    let _dns: fn(&mut Engine, PendingResolve) -> Result<nbreq::ResolveCompletion, nbreq::Error> =
        drive_with_public_bound;
    let _tcp: fn(
        &mut Engine,
        PendingTcpConnect,
    ) -> Result<nbreq::TcpConnectCompletion, nbreq::Error> = drive_with_public_bound;
}

#[test]
#[cfg(feature = "native")]
fn resolver_tickets_are_live_on_native_engines_and_tcp_stays_unwired() {
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
    let connect_error = tcp
        .submit(connect)
        .expect_err("standalone TCP must reject before admission");
    assert_eq!(connect_error.kind(), ErrorKind::Unsupported);

    let after = engine.metrics();
    assert_eq!(before, after);
    engine.shutdown().expect("empty Engine must stop");
}
