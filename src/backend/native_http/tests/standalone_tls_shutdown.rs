//! Exercise the actual owner failure/public-reader path at the final TLS shutdown syscall.
use super::*;

fn authenticate_available_close(
    server: &mut ServerConnection,
    socket: &mut std::net::TcpStream,
    observed: &mut bool,
) {
    loop {
        match server.read_tls(socket) {
            Ok(0) => return,
            Ok(_) => {
                let state = server
                    .process_new_packets()
                    .expect("peer authenticates client alert");
                *observed |= state.peer_has_closed();
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                return;
            }
            Err(error) => panic!("read actual client close record: {error}"),
        }
    }
}

fn exercise_final_shutdown(peer_close: bool, injected: std::io::ErrorKind, expect_success: bool) {
    let (client, mut server) = completed_tls12_pair();
    let (engine, mut backend, slot, mut connection, mut live, mut socket) = owner_fixture(client);
    socket.set_nonblocking(true).expect("bounded peer reads");

    // Use real authenticated application and alert records, not fabricated owner flags.
    server
        .writer()
        .write_all(b"pong")
        .expect("server application data");
    if peer_close {
        server.send_close_notify();
    }
    let mut wire = Vec::new();
    while server.wants_write() {
        server.write_tls(&mut wire).expect("server records");
    }
    let progress = live
        .tls
        .receive(&wire)
        .expect("authenticated server records");
    assert_eq!(progress.peer_closed, peer_close);
    assert_eq!(progress.plaintext.as_ref(), b"pong");
    backend
        .apply_standalone_tls_progress(slot, &mut live, progress)
        .expect("apply actual TLS progress");
    if !peer_close {
        assert_eq!(
            connection.try_finish().expect("explicit local finish"),
            crate::TcpFinishStatus::Pending
        );
    }
    backend.standalone_tls_live.insert(slot, live);
    backend.reactor.fail_next_write_shutdown_for_test(injected);
    assert!(backend.reactor.write_shutdown_failure_pending_for_test());

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut peer_observed_local_close = false;
    let mut reached_drained_shutdown = false;
    while backend.reactor.write_shutdown_failure_pending_for_test() {
        assert!(Instant::now() < deadline, "final shutdown was not reached");
        let live = backend
            .standalone_tls_live
            .get(&slot)
            .expect("owner must survive until injected syscall");
        if live.local_close_notify
            && live.wire_pending.is_empty()
            && !live.tls.wants_write()
            && backend
                .reactor
                .outbound_is_empty(slot)
                .expect("reactor output state")
        {
            assert_eq!(live.transport.owner.send_occupancy(), 0);
            assert_eq!(live.plaintext_inflight, 0);
            assert_eq!(live.peer_close_notify, peer_close);
            reached_drained_shutdown = true;
        }
        // This is the production service entry: failures must go through
        // fail_standalone_tls and become visible on the public connection reader.
        backend.service_standalone_tls().expect("service TLS owner");
        if backend.reactor.write_shutdown_failure_pending_for_test() {
            let events = backend
                .reactor
                .poll(Instant::now() + Duration::from_millis(10))
                .expect("drain actual client alert");
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, NativeEvent::Failed(_, _))),
                "unexpected actual socket failure: {events:?}"
            );
        }
        authenticate_available_close(&mut server, &mut socket, &mut peer_observed_local_close);
    }
    assert!(
        reached_drained_shutdown,
        "the injected error must follow all application and close-alert output"
    );
    assert!(
        !backend.reactor.write_shutdown_failure_pending_for_test(),
        "one-shot syscall fault must be consumed"
    );
    while !peer_observed_local_close && Instant::now() < deadline {
        authenticate_available_close(&mut server, &mut socket, &mut peer_observed_local_close);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        peer_observed_local_close,
        "peer must authenticate the already-drained client close_notify"
    );

    if expect_success {
        let mut bytes = [0; 4];
        assert_eq!(
            connection
                .try_read(&mut bytes)
                .expect("authenticated payload must survive redundant shutdown"),
            crate::TcpRead::Data(4)
        );
        assert_eq!(&bytes, b"pong");
        assert_eq!(
            connection.try_read(&mut bytes).expect("authenticated EOF"),
            crate::TcpRead::Eof
        );
        assert_eq!(
            connection
                .try_finish()
                .expect("completed authenticated finish"),
            crate::TcpFinishStatus::Finished
        );
        let live = backend
            .standalone_tls_live
            .get(&slot)
            .expect("owner retained orderly completion");
        assert!(live.peer_close_notify && live.local_close_notify && live.write_shutdown);
        assert!(live.wire_pending.is_empty());
        assert_eq!(live.plaintext_inflight, 0);
        assert_eq!(live.transport.owner.send_occupancy(), 0);
        assert!(live.transport.owner.write_finished());
    } else {
        assert!(
            !backend.standalone_tls_live.contains_key(&slot),
            "genuine shutdown failure must remove the owner"
        );
        match connection.try_read(&mut [0; 4]) {
            Err(TcpStreamError::Failed(error)) => {
                assert_eq!(error.transport_stage(), Some(TransportStage::Send));
                assert!(
                    error.message().contains("native write shutdown failed"),
                    "{error:?}"
                );
            }
            other => panic!("shutdown failure must remain visible to the public reader: {other:?}"),
        }
        assert!(
            connection.try_finish().is_err(),
            "genuine failure cannot publish successful finish"
        );
    }
    backend.reactor.cancel(slot);
    drop(connection);
    engine.shutdown().expect("owner fixture Engine shutdown");
}

#[test]
fn final_tls_write_shutdown_not_connected_preserves_payload_eof_and_finish() {
    exercise_final_shutdown(true, std::io::ErrorKind::NotConnected, true);
}

#[test]
fn final_tls_write_shutdown_without_peer_alert_or_with_other_errors_still_aborts() {
    for (peer_close, error) in [
        (false, std::io::ErrorKind::NotConnected),
        (true, std::io::ErrorKind::ConnectionReset),
        (true, std::io::ErrorKind::BrokenPipe),
    ] {
        exercise_final_shutdown(peer_close, error, false);
    }
}
