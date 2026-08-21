use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::testing;
use crate::{
    Completion, DriveStatus, Engine, EngineConfig, ErrorKind, ExecuteError, Request, Response,
    ShutdownOutcome, WaitOutcome,
};

fn request() -> Request {
    Request::get("https://example.invalid/")
        .build()
        .expect("test request must build")
}

fn response(marker: u8) -> Completion {
    Completion::Completed(Response::new(200, Vec::new(), vec![marker]))
}

#[test]
fn metrics_follow_canonical_terminal_and_retained_callback_resources() {
    let (mut engine, controller) =
        testing::engine(EngineConfig::manual()).expect("deterministic Engine must construct");
    let client = engine.client();
    let (callback_tx, callback_rx) = mpsc::channel();
    let handle = client
        .start(request(), move |completion| {
            callback_tx
                .send(completion)
                .expect("test receiver must remain");
        })
        .expect("callback request must submit");

    let accepted = engine.metrics();
    assert_eq!(accepted.requests_accepted(), 1);
    assert_eq!(accepted.current().inflight_requests(), 1);
    assert_eq!(accepted.current().queued_commands(), 1);
    assert_eq!(accepted.high_water().inflight_requests(), 1);
    assert_eq!(accepted.high_water().queued_commands(), 1);

    assert!(controller.complete(handle.id(), response(7)));
    let terminal = engine.metrics();
    assert_eq!(terminal.requests_completed(), 1);
    assert_eq!(terminal.requests_failed(), 0);
    assert_eq!(terminal.requests_cancelled(), 0);
    assert_eq!(terminal.current().queued_callbacks(), 1);
    assert_eq!(terminal.current().inflight_requests(), 1);

    engine
        .drive(Instant::now() + Duration::from_millis(20))
        .expect("manual drive must drain command and callback queues");
    assert!(matches!(
        callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback must run"),
        Completion::Completed(_)
    ));
    let drained = engine.metrics();
    assert_eq!(drained.current().queued_commands(), 0);
    assert_eq!(drained.current().queued_callbacks(), 0);
    assert_eq!(drained.current().inflight_requests(), 0);
    assert_eq!(drained.high_water().queued_callbacks(), 1);

    engine.shutdown().expect("Engine must stop");
}

#[test]
fn cancel_complete_race_has_exactly_one_terminal_winner() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();

    for marker in 0..100_u8 {
        let pending = client.submit(request()).expect("request must submit");
        let handle = pending.handle();
        let id = handle.id();
        let barrier = Arc::new(Barrier::new(3));

        let complete_barrier = Arc::clone(&barrier);
        let complete_controller = controller.clone();
        let completer = thread::spawn(move || {
            complete_barrier.wait();
            complete_controller.complete(id, response(marker))
        });

        let cancel_barrier = Arc::clone(&barrier);
        let cancel_handle = handle.clone();
        let canceller = thread::spawn(move || {
            cancel_barrier.wait();
            cancel_handle.cancel().expect("cancellation must submit");
        });

        barrier.wait();
        let completion = pending.wait();
        let completion_won = completer.join().expect("completer must not panic");
        canceller.join().expect("canceller must not panic");

        match completion {
            Completion::Completed(result) => {
                assert!(completion_won);
                assert_eq!(result.body(), &[marker]);
            }
            Completion::Cancelled => assert!(!completion_won),
            Completion::Failed(error) => panic!("unexpected terminal failure: {error}"),
        }
        assert!(!controller.complete(id, response(marker)));
        handle.cancel().expect("late cancellation must be harmless");
    }

    engine.shutdown().expect("Engine must stop");
}

#[test]
fn callback_request_receives_exactly_one_terminal_notification() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();
    let (completion_tx, completion_rx) = mpsc::channel();
    let handle = client
        .start(request(), move |completion| {
            completion_tx
                .send(completion)
                .expect("test receiver must remain");
        })
        .expect("callback request must submit");
    let id = handle.id();
    let barrier = Arc::new(Barrier::new(3));
    let complete_barrier = Arc::clone(&barrier);
    let complete_controller = controller.clone();
    let completer = thread::spawn(move || {
        complete_barrier.wait();
        complete_controller.complete(id, response(9));
    });
    let cancel_barrier = Arc::clone(&barrier);
    let cancel_handle = handle.clone();
    let canceller = thread::spawn(move || {
        cancel_barrier.wait();
        cancel_handle.cancel().expect("cancellation must submit");
    });

    barrier.wait();
    let _completion = completion_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("one terminal callback must run");
    completer.join().expect("completer must not panic");
    canceller.join().expect("canceller must not panic");
    assert!(
        completion_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err()
    );
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn dropping_plain_handle_does_not_cancel_but_cancel_guard_does() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();
    let (completion_tx, completion_rx) = mpsc::channel();
    let handle = client
        .start(request(), move |completion| {
            completion_tx
                .send(completion)
                .expect("test receiver must remain");
        })
        .expect("callback request must submit");
    let id = handle.id();
    drop(handle);
    assert!(controller.complete(id, response(4)));
    assert!(matches!(
        completion_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("dropped plain handle request must continue"),
        Completion::Completed(_)
    ));

    let pending = client
        .submit(request())
        .expect("guarded request must submit");
    let guarded_id = pending.handle().id();
    drop(pending.handle().cancel_on_drop());
    assert!(matches!(pending.wait(), Completion::Cancelled));
    assert!(!controller.complete(guarded_id, response(5)));
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn blocked_callback_does_not_delay_direct_waiter() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let callback = client
        .start(request(), move |_completion| {
            started_tx.send(()).expect("test receiver must remain");
            release_rx.recv().expect("test release must arrive");
        })
        .expect("callback request must submit");
    assert!(controller.complete(callback.id(), response(1)));
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("callback must begin");

    let pending = client
        .submit(request())
        .expect("waited request must submit");
    assert!(controller.complete(pending.handle().id(), response(2)));
    match pending.wait_for(Duration::from_millis(100)) {
        WaitOutcome::Completed(Completion::Completed(result)) => assert_eq!(result.body(), &[2]),
        other => panic!("waiter was delayed by callback dispatch: {other:?}"),
    }

    release_tx.send(()).expect("callback must remain alive");
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn callback_can_submit_and_complete_another_request() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();
    let callback_client = client.clone();
    let callback_controller = controller.clone();
    let (nested_tx, nested_rx) = mpsc::channel();

    let outer = client
        .start(request(), move |_completion| {
            let nested = callback_client
                .start(request(), move |_completion| {
                    nested_tx.send(()).expect("test receiver must remain");
                })
                .expect("callback reentrant submission must succeed");
            assert!(callback_controller.complete(nested.id(), response(2)));
        })
        .expect("outer request must submit");
    assert!(controller.complete(outer.id(), response(1)));
    nested_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("nested callback must run");

    engine.shutdown().expect("Engine must stop");
}

#[test]
fn callback_panic_and_forbidden_wait_do_not_kill_dispatcher() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();
    let pending = client
        .submit(request())
        .expect("pending request must submit");

    let panicking = client
        .start(request(), move |_completion| {
            let _never = pending.wait();
        })
        .expect("panicking callback request must submit");
    assert!(controller.complete(panicking.id(), response(1)));

    let (survivor_tx, survivor_rx) = mpsc::channel();
    let survivor = client
        .start(request(), move |_completion| {
            survivor_tx.send(()).expect("test receiver must remain");
        })
        .expect("survivor request must submit");
    assert!(controller.complete(survivor.id(), response(2)));
    survivor_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("dispatcher must survive callback panic");

    engine.shutdown().expect("Engine must stop");
}

#[test]
fn manual_callbacks_run_inline_only_when_driven() {
    let (mut engine, controller) =
        testing::engine(EngineConfig::manual()).expect("manual Engine must construct");
    let client = engine.client();
    let driving_thread = thread::current().id();
    let (called_tx, called_rx) = mpsc::channel();

    let handle = client
        .start(request(), move |_completion| {
            called_tx
                .send(thread::current().id())
                .expect("test receiver must remain");
        })
        .expect("manual callback request must submit");
    assert!(controller.complete(handle.id(), response(1)));
    assert!(called_rx.try_recv().is_err());

    let status = engine
        .drive(Instant::now())
        .expect("manual drive must succeed");
    assert_eq!(status, DriveStatus::Progress);
    assert_eq!(
        called_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("manual callback must run"),
        driving_thread
    );
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn cancel_all_has_a_deterministic_acceptance_barrier() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();
    let before = client.submit(request()).expect("first request must submit");

    engine.cancel_all();
    assert!(matches!(before.wait(), Completion::Cancelled));

    let after = client.submit(request()).expect("later request must submit");
    let after_id = after.handle().id();
    let after = match after.wait_for(Duration::ZERO) {
        WaitOutcome::TimedOut(after) => after,
        other => panic!("post-barrier request was unexpectedly terminal: {other:?}"),
    };
    assert!(controller.complete(after_id, response(3)));
    assert!(matches!(after.wait(), Completion::Completed(_)));
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn bounded_admission_recovers_after_terminal_state() {
    let one = NonZeroUsize::new(1).expect("one is non-zero");
    let config = EngineConfig::manual()
        .with_max_inflight_requests(one)
        .with_command_queue_capacity(one)
        .with_callback_queue_capacity(one);
    let (mut engine, _controller) = testing::engine(config).expect("bounded Engine must construct");
    let client = engine.client();
    let first = client.submit(request()).expect("first request must submit");
    engine
        .drive(Instant::now())
        .expect("first request must reach backend ownership");

    let error = client
        .submit(request())
        .expect_err("second request must meet bounded admission");
    assert_eq!(error.kind(), ErrorKind::QueueFull);

    first.handle().cancel().expect("first request must cancel");
    engine
        .drive(Instant::now())
        .expect("manual Engine must reap cancellation");
    let second = client
        .submit(request())
        .expect("terminal state must release capacity");
    second
        .handle()
        .cancel()
        .expect("cleanup cancellation must work");
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn normal_shutdown_waits_but_timed_shutdown_detaches_callbacks() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("normal Engine must construct");
    let client = engine.client();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let handle = client
        .start(request(), move |_completion| {
            started_tx.send(()).expect("test receiver must remain");
            release_rx.recv().expect("release must arrive");
        })
        .expect("callback request must submit");
    assert!(controller.complete(handle.id(), response(1)));
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("callback must begin");

    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let shutdown_thread = thread::spawn(move || {
        shutdown_tx
            .send(engine.shutdown())
            .expect("test receiver must remain");
    });
    assert!(shutdown_rx.recv_timeout(Duration::from_millis(50)).is_err());
    release_tx.send(()).expect("callback must remain alive");
    shutdown_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("shutdown must finish after callback")
        .expect("normal shutdown must succeed");
    shutdown_thread.join().expect("shutdown thread must join");

    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("timed Engine must construct");
    let client = engine.client();
    let surviving_client = client.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let handle = client
        .start(request(), move |_completion| {
            started_tx.send(()).expect("test receiver must remain");
            release_rx.recv().expect("release must arrive");
        })
        .expect("callback request must submit");
    assert!(controller.complete(handle.id(), response(2)));
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("callback must begin");

    let detached = match engine
        .shutdown_for(Duration::ZERO)
        .expect("timed network shutdown must succeed")
    {
        ShutdownOutcome::CallbacksRemaining(detached) => detached,
        ShutdownOutcome::Complete => panic!("running callback cannot already be complete"),
    };
    let error = surviving_client
        .submit(request())
        .expect_err("stopped Client must reject new work");
    assert_eq!(error.kind(), ErrorKind::EngineStopped);
    assert!(!detached.is_complete());
    release_tx.send(()).expect("callback must remain alive");
    detached.wait().expect("detached callback must finish");
    assert!(detached.is_complete());
}

#[test]
fn shutdown_delivers_cancelled_callback_with_stopped_client() {
    let (engine, _controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();
    let callback_client = client.clone();
    let (result_tx, result_rx) = mpsc::channel();

    client
        .start(request(), move |completion| {
            let submission = callback_client
                .submit(request())
                .expect_err("captured Client must observe stopped Engine");
            result_tx
                .send((completion, submission.kind()))
                .expect("test receiver must remain");
        })
        .expect("request must submit");
    engine.shutdown().expect("Engine must stop");
    let (completion, error_kind) = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("shutdown callback must run");
    assert!(matches!(completion, Completion::Cancelled));
    assert_eq!(error_kind, ErrorKind::EngineStopped);
}

#[test]
fn callback_pressure_holds_bounded_admission_until_callback_returns() {
    let one = NonZeroUsize::new(1).expect("one is non-zero");
    let config = EngineConfig::spawned()
        .with_max_inflight_requests(one)
        .with_command_queue_capacity(one)
        .with_callback_queue_capacity(one);
    let (engine, controller) = testing::engine(config).expect("bounded Engine must construct");
    let client = engine.client();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let returned = Arc::new(AtomicBool::new(false));
    let callback_returned = Arc::clone(&returned);

    let first = client
        .start(request(), move |_completion| {
            started_tx.send(()).expect("test receiver must remain");
            release_rx.recv().expect("release must arrive");
            callback_returned.store(true, Ordering::Release);
        })
        .expect("first callback must submit");
    assert!(controller.complete(first.id(), response(1)));
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("callback must begin");
    let error = client
        .submit(request())
        .expect_err("running callback must retain its bounded permit");
    assert_eq!(error.kind(), ErrorKind::QueueFull);

    release_tx.send(()).expect("callback must remain alive");
    let deadline = Instant::now() + Duration::from_secs(1);
    let second = loop {
        match client.submit(request()) {
            Ok(pending) => break pending,
            Err(error) if error.kind() == ErrorKind::QueueFull && Instant::now() < deadline => {
                thread::yield_now();
            }
            Err(error) => panic!("capacity did not recover: {error}"),
        }
    };
    assert!(returned.load(Ordering::Acquire));
    second
        .handle()
        .cancel()
        .expect("cleanup cancellation must work");
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn inflight_and_callback_event_bounds_are_independent_and_strict() {
    let one = NonZeroUsize::new(1).expect("one is non-zero");
    let two = NonZeroUsize::new(2).expect("two is non-zero");
    let config = EngineConfig::spawned()
        .with_max_inflight_requests(two)
        .with_command_queue_capacity(two)
        .with_callback_queue_capacity(one);
    let (engine, controller) = testing::engine(config).expect("bounded Engine must construct");
    let client = engine.client();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let callback = client
        .start(request(), move |_completion| {
            started_tx.send(()).expect("test receiver must remain");
            release_rx.recv().expect("release must arrive");
        })
        .expect("first callback request must submit");
    assert!(controller.complete(callback.id(), response(1)));
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("callback must begin");

    let callback_error = client
        .start(request(), |_completion| {})
        .expect_err("callback-event capacity must reject a second callback request");
    assert_eq!(callback_error.kind(), ErrorKind::QueueFull);

    let pending = client
        .submit(request())
        .expect("blocking-only traffic may use the remaining inflight slot");
    let inflight_error = client
        .submit(request())
        .expect_err("the explicit total inflight bound must remain strict");
    assert_eq!(inflight_error.kind(), ErrorKind::QueueFull);

    assert!(controller.complete(pending.handle().id(), response(2)));
    assert!(matches!(pending.wait(), Completion::Completed(_)));
    release_tx.send(()).expect("callback must remain alive");
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn inflight_limit_rejection_maps_through_callback_and_execute_forms() {
    let one = NonZeroUsize::new(1).expect("one is non-zero");
    let config = EngineConfig::spawned().with_max_inflight_requests(one);
    let (engine, _controller) = testing::engine(config).expect("bounded Engine must construct");
    let client = engine.client();
    let first = client.submit(request()).expect("first request must submit");

    let callback_error = client
        .start(request(), |_completion| {})
        .expect_err("callback form must expose admission pressure");
    assert_eq!(callback_error.kind(), ErrorKind::QueueFull);
    match client
        .execute(request())
        .expect_err("execute must expose the same admission pressure")
    {
        ExecuteError::Submission(error) => assert_eq!(error.kind(), ErrorKind::QueueFull),
        other => panic!("expected execute submission error, got {other:?}"),
    }

    first
        .handle()
        .cancel()
        .expect("cleanup cancellation must work");
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn command_queue_capacity_does_not_cap_inflight_requests_after_drain() {
    let one = NonZeroUsize::new(1).expect("one is non-zero");
    let two = NonZeroUsize::new(2).expect("two is non-zero");
    let config = EngineConfig::manual()
        .with_max_inflight_requests(two)
        .with_command_queue_capacity(one);
    let (mut engine, controller) = testing::engine(config).expect("bounded Engine must construct");
    let client = engine.client();

    let first = client.submit(request()).expect("first request must queue");
    let queue_error = client
        .submit(request())
        .expect_err("a full command queue must reject before it is drained");
    assert_eq!(queue_error.kind(), ErrorKind::QueueFull);
    engine
        .drive(Instant::now())
        .expect("manual drive must drain the first command");

    let second = client
        .submit(request())
        .expect("draining must reopen the one-slot command queue");
    engine
        .drive(Instant::now())
        .expect("manual drive must drain the second command");
    assert_eq!(controller.active_requests(), 2);
    let inflight_error = client
        .submit(request())
        .expect_err("the independent two-request inflight bound must now reject");
    assert_eq!(inflight_error.kind(), ErrorKind::QueueFull);

    first
        .handle()
        .cancel()
        .expect("first cleanup cancellation must work");
    second
        .handle()
        .cancel()
        .expect("second cleanup cancellation must work");
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn manual_drive_until_uses_canonical_completion() {
    let mut engine = Engine::with_backend(EngineConfig::manual(), crate::backend::scaffold())
        .expect("manual Engine must construct");
    let pending = engine
        .client()
        .submit(request())
        .expect("request must be accepted");
    let completion = engine
        .drive_until(pending)
        .expect("manual drive_until must progress request");
    assert!(matches!(completion, Completion::Failed(_)));
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn manual_drive_until_rejects_another_engines_pending_request() {
    let (mut first, _first_controller) =
        testing::engine(EngineConfig::manual()).expect("first Engine must construct");
    let (second, _second_controller) =
        testing::engine(EngineConfig::manual()).expect("second Engine must construct");
    let pending = second
        .client()
        .submit(request())
        .expect("request must submit");
    let error = first
        .drive_until(pending)
        .expect_err("cross-Engine drive_until must fail closed");
    assert_eq!(error.kind(), ErrorKind::WrongEngine);
    first.shutdown().expect("first Engine must stop");
    second.shutdown().expect("second Engine must stop");
}

#[test]
fn manual_command_driven_drive_waits_and_wakes_without_spinning() {
    let (mut idle_engine, _idle_controller) =
        testing::engine(EngineConfig::manual()).expect("idle Engine must construct");
    let started = Instant::now();
    let status = idle_engine
        .drive(started + Duration::from_millis(30))
        .expect("idle manual drive must return");
    assert_eq!(status, crate::DriveStatus::DeadlineReached);
    assert!(started.elapsed() >= Duration::from_millis(20));
    idle_engine.shutdown().expect("idle Engine must stop");

    let (mut engine, controller) =
        testing::engine(EngineConfig::manual()).expect("manual Engine must construct");
    let pending = engine
        .client()
        .submit(request())
        .expect("request must submit");
    let id = pending.handle().id();
    let completion_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        assert!(controller.complete(id, response(1)));
    });
    let completion = engine
        .drive_until(pending)
        .expect("manual drive_until must wake for completion");
    assert!(matches!(completion, Completion::Completed(_)));
    completion_thread
        .join()
        .expect("completion thread must join");
    engine.shutdown().expect("manual Engine must stop");
}

#[test]
fn blocking_execute_maps_engine_cancellation_distinctly() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("Engine must construct");
    let client = engine.client();
    let execute_thread = thread::spawn(move || client.execute(request()));
    let deadline = Instant::now() + Duration::from_secs(1);
    while controller.active_requests() == 0 && Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(controller.active_requests(), 1);

    engine.cancel_all();
    let result = execute_thread.join().expect("execute thread must join");
    assert!(matches!(result, Err(ExecuteError::Cancelled)));
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn engine_shutdown_releases_blocked_execute_caller() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("Engine must construct");
    let client = engine.client();
    let execute_thread = thread::spawn(move || client.execute(request()));
    let deadline = Instant::now() + Duration::from_secs(1);
    while controller.active_requests() == 0 && Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(controller.active_requests(), 1);

    engine.shutdown().expect("Engine must stop");
    let result = execute_thread.join().expect("execute thread must join");
    assert!(matches!(result, Err(ExecuteError::Cancelled)));
}

#[test]
fn waiter_local_timeout_retains_the_live_request() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("Engine must construct");
    let pending = engine
        .client()
        .submit(request())
        .expect("request must submit");
    let id = pending.handle().id();
    let pending = match pending.wait_for(Duration::from_millis(10)) {
        WaitOutcome::TimedOut(pending) => pending,
        other => panic!("local wait unexpectedly changed request state: {other:?}"),
    };
    assert!(!pending.is_complete());
    assert!(controller.complete(id, response(9)));
    assert!(matches!(pending.wait(), Completion::Completed(_)));
    engine.shutdown().expect("Engine must stop");
}

#[test]
fn synchronous_shutdown_from_callback_is_rejected_without_deadlock() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let client = engine.client();
    let owner = Arc::new(Mutex::new(Some(engine)));
    let callback_owner = Arc::clone(&owner);
    let (result_tx, result_rx) = mpsc::channel();

    let handle = client
        .start(request(), move |_completion| {
            let engine = callback_owner
                .lock()
                .expect("test owner must not poison")
                .take()
                .expect("callback must own Engine");
            let error = engine
                .shutdown()
                .expect_err("callback-stack shutdown must be rejected");
            result_tx
                .send(error.error().kind())
                .expect("test receiver must remain");
        })
        .expect("callback request must submit");
    assert!(controller.complete(handle.id(), response(1)));
    assert_eq!(
        result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback shutdown attempt must return"),
        ErrorKind::ReentrantOperation
    );
    let error = client
        .submit(request())
        .expect_err("deferred cleanup must close admission");
    assert_eq!(error.kind(), ErrorKind::EngineStopped);
}

#[test]
fn callback_stack_shutdown_waits_for_concurrent_start_activation_before_sealing() {
    let (engine, controller) =
        testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
    let shared = engine.shared_for_testing();
    let client = engine.client();
    let owner = Arc::new(Mutex::new(Some(engine)));
    let callback_owner = Arc::clone(&owner);
    let (shutdown_result_tx, shutdown_result_rx) = mpsc::channel();

    let first = client
        .start(request(), move |_completion| {
            let engine = callback_owner
                .lock()
                .expect("test owner must not poison")
                .take()
                .expect("callback must own Engine");
            let result = engine
                .shutdown()
                .expect_err("callback-stack shutdown must be rejected");
            shutdown_result_tx
                .send(result.error().kind())
                .expect("test receiver must remain");
        })
        .expect("first callback request must submit");

    let (activation_entered_tx, activation_entered_rx) = mpsc::channel();
    let (release_activation_tx, release_activation_rx) = mpsc::channel();
    shared.set_callback_activation_hook(move || {
        activation_entered_tx
            .send(())
            .expect("test receiver must remain");
        release_activation_rx
            .recv()
            .expect("activation must be released");
    });

    let second_client = client.clone();
    let (second_completion_tx, second_completion_rx) = mpsc::channel();
    let concurrent_start = thread::spawn(move || {
        second_client
            .start(request(), move |completion| {
                second_completion_tx
                    .send(completion)
                    .expect("test receiver must remain");
            })
            .map(|_handle| ())
    });
    activation_entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("concurrent start must pause during callback activation");

    assert!(controller.complete(first.id(), response(1)));
    assert_eq!(
        shutdown_result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback shutdown must return"),
        ErrorKind::ReentrantOperation
    );
    release_activation_tx
        .send(())
        .expect("activation must remain alive");
    concurrent_start
        .join()
        .expect("start thread must not panic")
        .expect("already-admitted start must finish without a sealed-queue panic");
    assert!(matches!(
        second_completion_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("accepted callback must receive its shutdown result"),
        Completion::Cancelled
    ));
}

#[test]
fn simultaneous_submissions_never_exceed_the_admission_limit() {
    const CONTENDERS: usize = 16;
    let one = NonZeroUsize::new(1).expect("one is non-zero");
    let config = EngineConfig::spawned()
        .with_max_inflight_requests(one)
        .with_command_queue_capacity(one)
        .with_callback_queue_capacity(one);
    let (engine, controller) = testing::engine(config).expect("bounded Engine must construct");
    let client = engine.client();
    let barrier = Arc::new(Barrier::new(CONTENDERS + 1));
    let mut contenders = Vec::new();

    for _ in 0..CONTENDERS {
        let contender_client = client.clone();
        let contender_barrier = Arc::clone(&barrier);
        contenders.push(thread::spawn(move || {
            contender_barrier.wait();
            contender_client.submit(request())
        }));
    }
    barrier.wait();

    let mut accepted = Vec::new();
    let mut rejected = 0;
    for contender in contenders {
        match contender.join().expect("contender must not panic") {
            Ok(pending) => accepted.push(pending),
            Err(error) => {
                assert_eq!(error.kind(), ErrorKind::QueueFull);
                rejected += 1;
            }
        }
    }
    assert_eq!(accepted.len(), 1);
    assert_eq!(rejected, CONTENDERS - 1);
    assert_eq!(controller.active_requests(), 1);

    accepted
        .pop()
        .expect("one request must be accepted")
        .handle()
        .cancel()
        .expect("accepted request must cancel");
    engine.shutdown().expect("Engine must stop");
}
