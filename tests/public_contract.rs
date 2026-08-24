#[cfg(feature = "native")]
use std::time::Instant;

use nbreq::EngineBuilder;
use nbreq::{
    Client, DetachedCallbacks, Engine, EngineConfig, EngineMetrics, ErrorKind, HttpBackend,
    PendingRequest, RequestHandle, ResourceMetrics, ResponseReader, StreamRequest, UploadBody,
    UploadSender,
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
