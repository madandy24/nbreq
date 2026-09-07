use std::io::Cursor;
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection};

use super::*;
use crate::TlsVerification;
use crate::backend::native::NativeReactor;
use crate::backend::native_tls::{NativeTls, NativeTlsConfigs};

struct Gate {
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = self.release.send(());
    }
}

fn handshake() -> (NativeTls, Vec<u8>, Gate) {
    let key = KeyPair::generate().expect("key");
    let cert = CertificateParams::new(vec!["worker.test".into()])
        .expect("parameters")
        .self_signed(&key)
        .expect("certificate");
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .expect("server config");
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let configs = NativeTlsConfigs::with_test_root_and_verification_gate(
        cert.der().clone(),
        entered_tx,
        release_rx,
    )
    .expect("client config");
    let mut client = configs
        .connection("worker.test", TlsVerification::Verify, vec![4; 1024 * 1024])
        .expect("client");
    let mut server = ServerConnection::new(Arc::new(config)).expect("server");
    server
        .read_tls(&mut Cursor::new(client.start().expect("ClientHello")))
        .expect("read hello");
    server.process_new_packets().expect("process hello");
    let mut flight = Vec::new();
    server.write_tls(&mut flight).expect("server flight");
    (client, flight, Gate { entered, release })
}

fn slot(reactor: &mut NativeReactor, listener: &TcpListener) -> SlotId {
    reactor
        .connect(listener.local_addr().expect("address"), None, 1, 1)
        .expect("slot")
}

fn observe(reactor: &mut NativeReactor, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !predicate() {
        assert!(Instant::now() < deadline, "worker observation timed out");
        reactor
            .poll((Instant::now() + Duration::from_millis(10)).min(deadline))
            .expect("poll");
    }
}

#[test]
fn saturation_and_cancel_keep_executing_jobs_charged_and_bodies_outside_workers() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let mut reactor = NativeReactor::new(8).expect("reactor");
    let mut workers = HandshakeWorkers::new(reactor.waker());
    assert!(
        workers.threads.is_empty(),
        "plaintext-only Engine must not spawn TLS workers"
    );
    let mut running = Vec::new();
    for _ in 0..WORKERS {
        let (mut client, flight, gate) = handshake();
        let id = slot(&mut reactor, &listener);
        let session = client.take_handshake().expect("session");
        assert_eq!(
            client
                .request
                .as_ref()
                .expect("body stays with owner")
                .bytes
                .capacity(),
            1024 * 1024
        );
        workers.submit(id, session, flight, None).expect("submit");
        gate.entered
            .recv_timeout(Duration::from_secs(2))
            .expect("verifier entered");
        drop(client); // Entire request allocation is dropped while its verifier remains gated.
        running.push((id, gate));
    }
    let mut queued = Vec::new();
    for _ in 0..QUEUED {
        let (mut client, flight, gate) = handshake();
        let id = slot(&mut reactor, &listener);
        workers
            .submit(id, client.take_handshake().expect("session"), flight, None)
            .expect("queue");
        queued.push((id, gate));
    }
    assert!(!workers.has_capacity());
    assert_eq!(
        workers.shared.occupied.load(Ordering::Acquire),
        WORKERS + QUEUED
    );
    for (id, _) in &running {
        workers.cancel(*id);
    }
    assert!(
        !workers.has_capacity(),
        "running cancellation refunded occupied jobs"
    );
    for (id, gate) in &queued {
        workers.cancel(*id);
        assert!(
            gate.entered.try_recv().is_err(),
            "cancelled queued verification was invoked"
        );
    }
    assert_eq!(workers.shared.occupied.load(Ordering::Acquire), WORKERS);
    // Repeated replacement cannot create replacement workers for cancelled running checks.
    for _ in 0..8 {
        let (mut client, flight, gate) = handshake();
        let id = slot(&mut reactor, &listener);
        workers
            .submit(id, client.take_handshake().expect("session"), flight, None)
            .expect("replacement");
        workers.cancel(id);
        assert!(gate.entered.try_recv().is_err());
        assert_eq!(workers.shared.occupied.load(Ordering::Acquire), WORKERS);
        assert_eq!(workers.threads.len(), WORKERS);
    }
    drop(running);
    observe(&mut reactor, || {
        workers.shared.occupied.load(Ordering::Acquire) == 0
    });
    assert!(
        workers.take_finished().is_none(),
        "cancelled job published a late result"
    );
    workers.shutdown().expect("join");
}

#[test]
fn uncollected_results_hold_capacity_and_cancel_releases_them() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let mut reactor = NativeReactor::new(8).expect("reactor");
    let mut workers = HandshakeWorkers::new(reactor.waker());
    let mut ids = Vec::new();
    for count in 1..=WORKERS + QUEUED {
        let (mut client, _, _gate) = handshake();
        let id = slot(&mut reactor, &listener);
        workers
            .submit(
                id,
                client.take_handshake().expect("session"),
                Vec::new(),
                None,
            )
            .expect("empty step");
        observe(&mut reactor, || {
            workers.shared.state.lock().expect("state").finished.len() == count
        });
        ids.push(id);
    }
    assert!(
        !workers.has_capacity(),
        "completed TLS state escaped the job bound"
    );
    for id in ids {
        workers.cancel(id);
    }
    assert_eq!(workers.shared.occupied.load(Ordering::Acquire), 0);
    assert!(workers.take_finished().is_none());
    assert!(workers.has_capacity());
    workers.shutdown().expect("join");
}

#[test]
fn shutdown_discards_queued_jobs_and_waits_only_for_executing_verifiers() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let mut reactor = NativeReactor::new(8).expect("reactor");
    let mut workers = HandshakeWorkers::new(reactor.waker());
    let mut gates = Vec::new();
    for index in 0..WORKERS + QUEUED {
        let (mut client, flight, gate) = handshake();
        workers
            .submit(
                slot(&mut reactor, &listener),
                client.take_handshake().expect("session"),
                flight,
                None,
            )
            .expect("submit");
        if index < WORKERS {
            gate.entered
                .recv_timeout(Duration::from_secs(2))
                .expect("running");
        }
        gates.push(gate);
    }
    let shared = Arc::clone(&workers.shared);
    let shutdown = thread::spawn(move || workers.shutdown());
    observe(&mut reactor, || {
        shared.state.lock().expect("state").stopping
            && shared.occupied.load(Ordering::Acquire) == WORKERS
    });
    assert!(
        !shutdown.is_finished(),
        "shutdown abandoned executing verification"
    );
    for gate in &gates[WORKERS..] {
        assert!(gate.entered.try_recv().is_err());
    }
    drop(gates);
    shutdown
        .join()
        .expect("shutdown thread")
        .expect("workers joined");
    assert_eq!(shared.occupied.load(Ordering::Acquire), 0);
    assert!(shared.state.lock().expect("state").finished.is_empty());
}

#[test]
fn oversized_handshake_step_is_rejected_before_worker_startup() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let mut reactor = NativeReactor::new(8).expect("reactor");
    let mut workers = HandshakeWorkers::new(reactor.waker());
    let (mut client, _, _gate) = handshake();
    assert!(
        workers
            .submit(
                slot(&mut reactor, &listener),
                client.take_handshake().expect("session"),
                vec![0; INPUT_WINDOW + 1],
                None
            )
            .is_err()
    );
    assert!(workers.threads.is_empty());
    assert_eq!(workers.shared.occupied.load(Ordering::Acquire), 0);
}

#[test]
fn expired_queued_handshake_does_not_enter_certificate_verification() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let mut reactor = NativeReactor::new(8).expect("reactor");
    let mut workers = HandshakeWorkers::new(reactor.waker());
    let mut running = Vec::new();
    for _ in 0..WORKERS {
        let (mut client, flight, gate) = handshake();
        let id = slot(&mut reactor, &listener);
        workers
            .submit(id, client.take_handshake().expect("session"), flight, None)
            .expect("submit");
        gate.entered
            .recv_timeout(Duration::from_secs(2))
            .expect("running");
        running.push((id, gate));
    }
    let (mut client, flight, gate) = handshake();
    let id = slot(&mut reactor, &listener);
    let deadline = Instant::now() + Duration::from_millis(20);
    workers
        .submit(
            id,
            client.take_handshake().expect("session"),
            flight,
            Some(deadline),
        )
        .expect("queue");
    while Instant::now() < deadline {
        reactor.poll(deadline).expect("deadline observation");
    }
    for (id, _) in &running {
        workers.cancel(*id);
    }
    drop(running);
    observe(&mut reactor, || {
        !workers
            .shared
            .state
            .lock()
            .expect("state")
            .finished
            .is_empty()
    });
    let result = workers.take_finished().expect("expired result");
    assert_eq!(result.slot, id);
    assert!(matches!(&result.progress, Err(error) if error.kind() == ErrorKind::Timeout));
    assert!(
        gate.entered.try_recv().is_err(),
        "expired job entered verification"
    );
    drop(result);
    workers.shutdown().expect("join");
}
