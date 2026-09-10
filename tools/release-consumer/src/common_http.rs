//! The exact same consumer source runs against registry 0.1.1 and the 0.2 package.
use std::io::Read;
use std::sync::mpsc;

use super::fixture::{Server, WAIT, read_head};
use nbreq::{Completion, Engine, EngineConfig, Request};

#[test]
fn explicit_http_borrow_and_clone_survive_shutdown() {
    let server = Server::http(b"ordinary HTTP consumer");
    let engine = Engine::new(EngineConfig::spawned()).expect("ordinary Engine");
    let client = engine.client();
    let response = client
        .execute(
            Request::get(server.url())
                .total_timeout(WAIT)
                .build()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(response.status(), 200);
    let clone = response.clone();
    drop(response);
    engine.shutdown().expect("joined shutdown");
    assert_eq!(clone.body(), b"ordinary HTTP consumer");
    assert!(
        client
            .submit(Request::get(server.url()).build().unwrap())
            .is_err()
    );
    server.finish();
}

#[test]
fn manual_waiter_is_driven_by_its_owner() {
    let server = Server::http(b"manual HTTP consumer");
    let mut engine = Engine::new(EngineConfig::manual()).unwrap();
    let pending = engine
        .client()
        .submit(
            Request::get(server.url())
                .total_timeout(WAIT)
                .build()
                .unwrap(),
        )
        .unwrap();
    let completion = engine.drive_until(pending).unwrap();
    engine.shutdown().unwrap();
    match completion {
        Completion::Completed(response) => assert_eq!(response.body(), b"manual HTTP consumer"),
        other => panic!("unexpected completion: {other:?}"),
    }
    server.finish();
}

#[test]
fn cancellation_delivers_one_callback_before_join_returns() {
    let (started, observed) = mpsc::sync_channel(1);
    let server = Server::new(move |mut socket| {
        read_head(&mut socket);
        started.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).expect("cancel closes socket"), 0);
    });
    let engine = Engine::new(EngineConfig::spawned()).unwrap();
    let (sender, receiver) = mpsc::sync_channel(2);
    let handle = engine
        .client()
        .start(
            Request::get(server.url())
                .total_timeout(WAIT)
                .build()
                .unwrap(),
            move |completion| {
                sender.send(completion).unwrap();
            },
        )
        .unwrap();
    observed.recv_timeout(WAIT).expect("request is on the wire");
    handle.cancel().unwrap();
    engine.shutdown().unwrap();
    assert!(matches!(
        receiver.recv_timeout(WAIT).unwrap(),
        Completion::Cancelled
    ));
    assert!(receiver.try_recv().is_err());
    server.finish();
}
