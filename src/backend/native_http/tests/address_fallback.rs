use super::super::*;
use std::net::TcpListener;

fn failure(kind: NativeFailureKind) -> NativeFailure {
    NativeFailure {
        kind,
        message: "controlled socket failure".into(),
        io_kind: Some(std::io::ErrorKind::ConnectionRefused),
    }
}

fn connecting(tls: bool) -> (NativeHttpBackend, RequestId, TcpListener, TcpListener) {
    let first = TcpListener::bind("127.0.0.1:0").expect("first listener");
    let second = TcpListener::bind("127.0.0.1:0").expect("second listener");
    let config = EngineConfig::manual();
    let configs = tls.then(|| {
        NativeTlsConfigs::with_test_root({
            let key = rcgen::KeyPair::generate().expect("key");
            rcgen::CertificateParams::new(vec!["fallback.test".into()])
                .expect("parameters")
                .self_signed(&key)
                .expect("certificate")
                .der()
                .clone()
        })
        .expect("TLS configs")
    });
    let mut backend = NativeHttpBackend::new(
        HttpLimits::from_config(&config),
        None,
        configs,
        ConnectionLimits::from_config(&config),
    )
    .expect("backend");
    let id = RequestId {
        engine: 1,
        sequence: 1,
    };
    let request = Request::post(format!(
        "{}://fallback.test/",
        if tls { "https" } else { "http" }
    ))
    .body(vec![17; 50 * 1024])
    .build()
    .expect("request");
    let deadline = Instant::now() + Duration::from_secs(5);
    let pending = match backend.make_pending(
        id,
        request,
        PendingDeadlines {
            connect: Some(deadline),
            total: Some(deadline),
            inactivity: None,
        },
        0,
        ErrorKind::InvalidRequest,
        PendingResponse::Buffered,
    ) {
        Ok(pending) => pending,
        Err((error, _)) => panic!("pending: {error}"),
    };
    assert!(backend.reserve_connection(&pending.key));
    assert!(
        backend
            .begin_connection(
                VecDeque::from([
                    first.local_addr().expect("first address"),
                    second.local_addr().expect("second address")
                ]),
                pending
            )
            .is_none()
    );
    (backend, id, first, second)
}

#[test]
fn fallback_preserves_body_deadlines_and_one_reservation_then_cancel_releases_it() {
    for tls in [false, true] {
        let (mut backend, id, _first, _second) = connecting(tls);
        let original = backend.request_to_slot[&id];
        let transfer = &backend.transfers[&original];
        let body = transfer.request.body().as_ptr();
        let wire = transfer
            .connecting
            .as_ref()
            .expect("connecting")
            .cleartext_request
            .as_ref()
            .map(|bytes| bytes.as_ptr());
        let deadlines = (transfer.connect_deadline, transfer.total_deadline);
        let mut completions = Vec::new();
        backend.fail_native(
            original,
            failure(NativeFailureKind::Connect),
            &mut completions,
        );
        let replacement = backend.request_to_slot[&id];
        assert_ne!(replacement, original);
        let transfer = &backend.transfers[&replacement];
        assert_eq!(
            transfer.request.body().as_ptr(),
            body,
            "address fallback copied the request body"
        );
        assert_eq!(
            transfer
                .connecting
                .as_ref()
                .expect("connecting")
                .cleartext_request
                .as_ref()
                .map(|bytes| bytes.as_ptr()),
            wire
        );
        assert_eq!(
            (transfer.connect_deadline, transfer.total_deadline),
            deadlines
        );
        assert_eq!(backend.connection_count, 1);
        assert_eq!(backend.reactor.active_count(), 1);
        assert!(
            backend
                .reactor
                .outbound_is_empty(replacement)
                .expect("outbound"),
            "request bytes sent before TCP connected"
        );
        backend.cancel(id);
        backend.fail_native(
            original,
            failure(NativeFailureKind::Connect),
            &mut completions,
        );
        backend.fail_native(
            replacement,
            failure(NativeFailureKind::Connect),
            &mut completions,
        );
        assert!(
            completions.is_empty(),
            "stale failures resurrected cancellation"
        );
        assert!(backend.transfers.is_empty());
        assert_eq!(backend.connection_count, 0);
        assert_eq!(backend.reactor.active_count(), 0);
        backend.shutdown().expect("shutdown");
    }
}

#[test]
fn fallback_exhaustion_and_expiry_complete_once_without_resetting_deadlines() {
    for expire in [false, true] {
        let (mut backend, id, _first, second) = connecting(false);
        let original = backend.request_to_slot[&id];
        let mut completions = Vec::new();
        if expire {
            backend
                .transfers
                .get_mut(&original)
                .expect("transfer")
                .connect_deadline = Some(Instant::now());
        }
        backend.fail_native(
            original,
            failure(NativeFailureKind::Connect),
            &mut completions,
        );
        if !expire {
            let next = backend.request_to_slot[&id];
            backend.fail_native(next, failure(NativeFailureKind::Connect), &mut completions);
            backend.fail_native(next, failure(NativeFailureKind::Connect), &mut completions);
        } else {
            second.set_nonblocking(true).expect("bounded accept");
            assert!(
                matches!(second.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        }
        assert_eq!(completions.len(), 1);
        match &completions[0].completion {
            Completion::Failed(error) if expire => {
                assert_eq!(error.timeout_kind(), Some(TimeoutKind::Connect))
            }
            Completion::Failed(error) => {
                assert_eq!(error.transport_stage(), Some(TransportStage::Connect))
            }
            other => panic!("unexpected completion: {other:?}"),
        }
        assert_eq!(backend.connection_count, 0);
        assert_eq!(backend.reactor.active_count(), 0);
        backend.shutdown().expect("shutdown");
    }
}

#[test]
fn fallback_never_replays_a_request_after_tcp_connects() {
    for tls in [false, true] {
        let (mut backend, id, _first, second) = connecting(tls);
        let slot = backend.request_to_slot[&id];
        let mut completions = Vec::new();
        backend
            .handle_connected(slot, true, &mut completions)
            .expect("connected");
        assert!(backend.transfers[&slot].connecting.is_none());
        backend.fail_native(slot, failure(NativeFailureKind::Read), &mut completions);
        assert_eq!(completions.len(), 1);
        assert_eq!(backend.connection_count, 0);
        second.set_nonblocking(true).expect("bounded accept");
        assert!(
            matches!(second.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        backend.shutdown().expect("shutdown");
    }
}
