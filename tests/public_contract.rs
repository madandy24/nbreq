use std::time::Instant;

use nbreq::{
    Client, DetachedCallbacks, Engine, EngineBuilder, EngineConfig, EngineMetrics, ErrorKind,
    HttpBackend, PendingRequest, RequestHandle, ResourceMetrics, ResponseReader, StreamRequest,
    UploadBody, UploadSender,
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
fn clients_are_engine_issued_and_do_not_own_shutdown() {
    let engine = Engine::new(EngineConfig::spawned()).expect("Engine must construct");
    let client = engine.client();
    let clone = client.clone();
    drop(client);
    drop(clone);
    engine.shutdown().expect("empty Engine must stop");
}

#[test]
fn metrics_are_owner_observed_and_start_empty() {
    let engine = Engine::new(EngineConfig::spawned()).expect("Engine must construct");
    let metrics = engine.metrics();
    assert_eq!(metrics, EngineMetrics::default());
    assert_eq!(metrics.current(), ResourceMetrics::default());
    engine.shutdown().expect("empty Engine must stop");
}

#[test]
#[cfg(feature = "native")]
fn explicit_native_selection_is_available_without_changing_the_default() {
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
fn explicit_native_selection_fails_when_unavailable() {
    let error = Engine::builder()
        .http_backend(HttpBackend::Native)
        .build()
        .err()
        .expect("uncompiled native backend must fail construction");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}

#[test]
#[cfg(feature = "curl-pilot")]
fn explicit_curl_selection_is_available_without_inventing_connection_metrics() {
    let engine = Engine::builder()
        .http_backend(HttpBackend::Curl)
        .build()
        .expect("compiled curl backend must construct explicitly");
    assert!(!engine.metrics().connection_metrics_available());
    engine.shutdown().expect("empty curl Engine must stop");
}

#[test]
#[cfg(not(feature = "curl-pilot"))]
fn explicit_curl_selection_fails_when_unavailable() {
    let error = Engine::builder()
        .http_backend(HttpBackend::Curl)
        .build()
        .err()
        .expect("uncompiled curl backend must fail construction");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}

#[test]
fn spawned_drive_fails_explicitly() {
    let mut engine = Engine::new(EngineConfig::spawned()).expect("Engine must construct");
    let error = engine
        .drive(Instant::now())
        .expect_err("spawned Engine must reject manual drive");
    assert_eq!(error.kind(), ErrorKind::WrongMode);
}

#[test]
#[cfg(not(feature = "curl-pilot"))]
fn manual_drive_uses_the_same_public_engine_type() {
    let mut engine = Engine::new(EngineConfig::manual()).expect("scaffold Engine must construct");
    let _status = engine
        .drive(Instant::now())
        .expect("manual scaffold drive must succeed");
    engine.shutdown().expect("empty Engine must stop");
}

#[test]
#[cfg(feature = "curl-pilot")]
fn curl_pilot_rejects_manual_construction_explicitly() {
    let error = Engine::new(EngineConfig::manual())
        .err()
        .expect("curl pilot must reject manual construction");
    assert_eq!(error.kind(), ErrorKind::WrongMode);
}
