use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::{Completion, EngineConfig, Header, Request, Response, WaitOutcome, testing};

fn request() -> Request {
    Request::get("https://example.invalid/body-ownership")
        .build()
        .expect("request must build")
}

fn completion() -> (usize, Completion) {
    let mut bytes = Vec::with_capacity(64 * 1024);
    bytes.resize(50 * 1024, 0x5a);
    let address = bytes.as_ptr() as usize;
    (
        address,
        Completion::Completed(Response::new(
            201,
            vec![Header::new("x-test", b"owned".to_vec())],
            bytes,
        )),
    )
}

fn response(completion: Completion) -> Response {
    let Completion::Completed(response) = completion else {
        panic!("expected completed response, got {completion:?}");
    };
    assert_eq!(response.status(), 201);
    assert_eq!(response.headers()[0].value(), b"owned");
    assert_eq!(response.body().len(), 50 * 1024);
    assert!(response.body().iter().all(|byte| *byte == 0x5a));
    response
}

fn assert_original_body(address: usize, completion: Completion) {
    let response = response(completion);
    assert_eq!(
        response.body().as_ptr() as usize,
        address,
        "delivery must preserve the original body allocation"
    );
    let bytes = response
        .into_body()
        .try_into_vec()
        .expect("delivery must leave no hidden body alias");
    assert_eq!(bytes.as_ptr() as usize, address);
    assert!(
        bytes.capacity() >= 64 * 1024,
        "delivery must preserve spare capacity"
    );
}

#[test]
fn m2_unique_body_extraction_preserves_empty_and_nonempty_allocations() {
    for (length, capacity) in [(0, 0), (0, 64), (1024, 2048), (50 * 1024, 64 * 1024)] {
        let mut bytes = Vec::with_capacity(capacity);
        bytes.resize(length, 0x61);
        let address = bytes.as_ptr();
        let capacity = bytes.capacity();
        let body = Response::new(200, Vec::new(), bytes).into_body();
        assert_eq!(body.as_bytes().len(), length);
        assert_eq!(body.as_ref(), body.as_bytes());
        let bytes = body.try_into_vec().expect("unique body must transfer");
        assert_eq!(bytes.as_ptr(), address);
        assert_eq!(bytes.capacity(), capacity);
        assert_eq!(bytes.len(), length);
        assert!(bytes.iter().all(|byte| *byte == 0x61));
    }
}

#[test]
fn m2_shared_extraction_returns_owner_until_last_alias_is_released() {
    let (address, completion) = completion();
    let original = response(completion);
    let body = original.clone().into_body();
    let alias = body.clone();
    let body = body.try_into_vec().expect_err("Response still shares body");
    assert_eq!(body.as_bytes().as_ptr() as usize, address);
    drop(original);
    let body = body
        .try_into_vec()
        .expect_err("ResponseBody still shares body");
    assert_eq!(body.as_bytes().as_ptr() as usize, address);
    drop(alias);
    let bytes = body
        .try_into_vec()
        .expect("last remaining owner must transfer");
    assert_eq!(bytes.as_ptr() as usize, address);
    assert!(bytes.capacity() >= 64 * 1024);
}

#[test]
fn m2_existing_borrow_and_copy_usage_keeps_independent_mutable_bytes() {
    let (_, completion) = completion();
    let original = response(completion);
    let mut copied = original.body().to_vec();
    copied[0] = 0x7f;
    assert_eq!(original.body()[0], 0x5a);
    let alias = original.clone().into_body();
    let copy_from_shared = original
        .into_body()
        .try_into_vec()
        .unwrap_or_else(|body| body.as_bytes().to_vec());
    assert_eq!(copy_from_shared, alias.as_bytes());
    assert_ne!(copy_from_shared.as_ptr(), alias.as_bytes().as_ptr());
    drop(alias);
    assert_eq!(copied[0], 0x7f);
}

#[test]
fn m2_response_body_equality_compares_bytes_not_capacity_or_owner() {
    let empty = Response::new(200, Vec::new(), Vec::new()).into_body();
    let allocated_empty = Response::new(200, Vec::new(), Vec::with_capacity(16)).into_body();
    assert_eq!(empty, allocated_empty);
    let mut bytes = Vec::with_capacity(64);
    bytes.extend_from_slice(b"same");
    let first = Response::new(200, Vec::new(), bytes);
    let second = Response::new(200, Vec::new(), b"same".to_vec());
    assert_eq!(first, second);
    assert_ne!(first.body().as_ptr(), second.body().as_ptr());
    assert_ne!(first.into_body(), empty);
}

#[test]
fn m2_body_sharing_across_threads_releases_unique_ownership_after_join() {
    let (address, completion) = completion();
    let body = response(completion).into_body();
    let alias = body.clone();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        release_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("release alias");
        assert_eq!(alias.as_bytes().as_ptr() as usize, address);
    });
    let body = body.try_into_vec().expect_err("worker owns an alias");
    release_tx.send(()).expect("release");
    worker.join().expect("body reader must join");
    let bytes = body.try_into_vec().expect("unique after join");
    assert_eq!(bytes.as_ptr() as usize, address);
}

#[test]
fn m2_response_clone_shares_immutable_body_storage() {
    let (address, completion) = completion();
    let original = response(completion);
    let cloned = original.clone();
    assert_eq!(original, cloned);
    assert_eq!(
        cloned.body().as_ptr() as usize,
        address,
        "cloning a response must share its body instead of copying it"
    );
}

#[test]
fn m2_wait_delivers_the_original_body_allocation() {
    let (engine, controller) = testing::engine(EngineConfig::spawned()).expect("Engine");
    let pending = engine.client().submit(request()).expect("submit");
    let handle = pending.handle();
    let (address, completion) = completion();
    assert!(controller.complete(handle.id(), completion));
    let delivered = pending.wait();
    handle.cancel().expect("late cancel");
    engine
        .shutdown()
        .expect("shutdown with result and handle held");
    assert_original_body(address, delivered);
}

#[test]
fn m2_timed_wait_delivers_original_body_after_local_timeout() {
    let (engine, controller) = testing::engine(EngineConfig::spawned()).expect("Engine");
    let pending = engine.client().submit(request()).expect("submit");
    let pending = match pending.wait_for(Duration::ZERO) {
        WaitOutcome::TimedOut(pending) => pending,
        WaitOutcome::Completed(_) => panic!("held request must remain pending"),
    };
    let (address, completion) = completion();
    assert!(controller.complete(pending.handle().id(), completion));
    let WaitOutcome::Completed(delivered) = pending.wait_for(Duration::ZERO) else {
        panic!("terminal result must be ready");
    };
    engine.shutdown().expect("shutdown");
    assert_original_body(address, delivered);
}

#[test]
fn m2_manual_drive_delivers_the_original_body_allocation() {
    let (mut engine, controller) = testing::engine(EngineConfig::manual()).expect("Engine");
    let pending = engine.client().submit(request()).expect("submit");
    let (address, completion) = completion();
    assert!(controller.complete(pending.handle().id(), completion));
    let delivered = engine.drive_until(pending).expect("drive until terminal");
    engine.shutdown().expect("shutdown");
    assert_original_body(address, delivered);
}

#[test]
fn m2_spawned_callback_delivers_the_original_body_allocation() {
    let (engine, controller) = testing::engine(EngineConfig::spawned()).expect("Engine");
    let (tx, rx) = mpsc::channel();
    let handle = engine
        .client()
        .start(request(), move |completion| {
            tx.send(completion).expect("receiver")
        })
        .expect("start");
    let (address, completion) = completion();
    assert!(controller.complete(handle.id(), completion));
    let delivered = rx.recv_timeout(Duration::from_secs(2)).expect("callback");
    engine.shutdown().expect("shutdown");
    assert_original_body(address, delivered);
}

#[test]
fn m2_queued_manual_callback_delivers_the_original_body_allocation() {
    let (mut engine, controller) = testing::engine(EngineConfig::manual()).expect("Engine");
    let (tx, rx) = mpsc::channel();
    let handle = engine
        .client()
        .start(request(), move |completion| {
            tx.send(completion).expect("receiver")
        })
        .expect("start");
    let (address, completion) = completion();
    assert!(controller.complete(handle.id(), completion));
    assert!(
        rx.try_recv().is_err(),
        "manual callback must wait for driving"
    );
    engine.drive(Instant::now()).expect("drive callback");
    let delivered = rx.recv_timeout(Duration::from_secs(2)).expect("callback");
    engine.shutdown().expect("shutdown");
    assert_original_body(address, delivered);
}
