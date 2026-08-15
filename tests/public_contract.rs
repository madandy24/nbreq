use std::time::Instant;

use nbreq::{
    Client, DetachedCallbacks, Engine, EngineConfig, ErrorKind, PendingRequest, RequestHandle,
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
}

#[test]
fn clients_are_engine_issued_and_do_not_own_shutdown() {
    let engine = Engine::new(EngineConfig::spawned()).expect("scaffold Engine must construct");
    let client = engine.client();
    let clone = client.clone();
    drop(client);
    drop(clone);
    engine.shutdown().expect("empty Engine must stop");
}

#[test]
fn spawned_drive_fails_explicitly() {
    let mut engine = Engine::new(EngineConfig::spawned()).expect("scaffold Engine must construct");
    let error = engine
        .drive(Instant::now())
        .expect_err("spawned Engine must reject manual drive");
    assert_eq!(error.kind(), ErrorKind::WrongMode);
}

#[test]
fn manual_drive_uses_the_same_public_engine_type() {
    let mut engine = Engine::new(EngineConfig::manual()).expect("scaffold Engine must construct");
    let _status = engine
        .drive(Instant::now())
        .expect("manual scaffold drive must succeed");
    engine.shutdown().expect("empty Engine must stop");
}

#[cfg(feature = "test-support")]
#[test]
fn late_cancel_is_idempotent_but_wrong_engine_fails_closed() {
    let first = Engine::new(EngineConfig::spawned()).expect("first Engine must construct");
    let first_client = first.client();
    let pending = nbreq::testing::completed(&first_client, 7, nbreq::Completion::Cancelled);
    let handle = pending.handle();

    let second = Engine::new(EngineConfig::spawned()).expect("second Engine must construct");
    let second_client = second.client();
    let error = second_client
        .cancel(handle.id())
        .expect_err("cross-Engine cancellation must fail closed");
    assert_eq!(error.kind(), ErrorKind::WrongEngine);

    first.shutdown().expect("first Engine must stop");
    handle
        .cancel()
        .expect("same-Engine cancellation remains harmless after stop");
    second.shutdown().expect("second Engine must stop");
}
