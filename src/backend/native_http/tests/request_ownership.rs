use super::super::*;
use crate::Method;
use std::io::{Read, Write};
use std::net::TcpListener;

const LARGE_BODY: usize = 2 * 1024 * 1024;

fn backend(tls: bool) -> NativeHttpBackend {
    let config = EngineConfig::manual();
    let configs = tls.then(|| {
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["ownership.test".into()])
            .expect("certificate parameters")
            .self_signed(&key)
            .expect("certificate");
        NativeTlsConfigs::with_test_root(cert.der().clone()).expect("TLS configs")
    });
    NativeHttpBackend::new(
        HttpLimits::from_config(&config),
        None,
        configs,
        ConnectionLimits::from_config(&config),
    )
    .expect("backend")
}

fn pending(backend: &NativeHttpBackend, tls: bool) -> (PendingResolve, *const u8) {
    let mut bytes = Vec::with_capacity(LARGE_BODY + 37);
    bytes.resize(LARGE_BODY, 0xa5);
    let pointer = bytes.as_ptr();
    let request = Request::post(format!(
        "{}://ownership.test/upload",
        if tls { "https" } else { "http" }
    ))
    .body(bytes)
    .build()
    .expect("request");
    let pending = backend
        .make_pending(
            RequestId {
                engine: 1,
                sequence: 1,
            },
            request,
            PendingDeadlines {
                connect: None,
                total: None,
                inactivity: None,
            },
            0,
            ErrorKind::InvalidRequest,
            PendingResponse::Buffered,
        )
        .unwrap_or_else(|(error, _)| panic!("pending request: {error}"));
    (pending, pointer)
}

fn connecting(tls: bool) -> (NativeHttpBackend, SlotId, TcpListener, *const u8) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let mut backend = backend(tls);
    let (pending, pointer) = pending(&backend, tls);
    let id = pending.request_id;
    assert!(backend.reserve_connection(&pending.key));
    assert!(
        backend
            .begin_connection(
                VecDeque::from([listener.local_addr().expect("address")]),
                pending
            )
            .is_none()
    );
    let slot = backend.request_to_slot[&id];
    (backend, slot, listener, pointer)
}

#[test]
fn m23_pending_request_keeps_body_out_of_serialized_storage() {
    for tls in [false, true] {
        let backend = backend(tls);
        let (pending, pointer) = pending(&backend, tls);
        assert_eq!(pending.request.body().as_ptr(), pointer);
        assert_eq!(pending.request.body().len(), LARGE_BODY);
        assert!(
            pending.serialized.bytes.capacity() < 1024,
            "DNS/connect preparation duplicated a whole body: {} bytes",
            pending.serialized.bytes.capacity()
        );
        assert!(pending.serialized.bytes.ends_with(b"\r\n\r\n"));
        assert!(
            pending
                .serialized
                .bytes
                .windows(b"Content-Length: 2097152".len())
                .any(|value| value == b"Content-Length: 2097152")
        );
    }
}

#[test]
fn m23_connection_waiter_keeps_only_original_body_storage() {
    let mut backend = backend(false);
    backend.connection_limits.global = 1;
    let (pending, pointer) = pending(&backend, false);
    let key = pending.key.clone();
    let id = pending.request_id;
    assert!(backend.reserve_connection(&key));
    assert!(backend.start_pending(pending).is_none());
    assert_eq!(backend.waiting.len(), 1);
    let waiting = &backend.waiting[0];
    assert_eq!(waiting.request.body().as_ptr(), pointer);
    assert!(
        waiting.serialized.bytes.capacity() < 1024,
        "connection admission retained a second full request body"
    );
    backend.cancel(id);
    assert!(backend.waiting.is_empty());
    backend.release_connection(&key);
    backend.shutdown().expect("shutdown");
}

#[test]
fn m23_cleartext_connecting_state_does_not_duplicate_the_body() {
    let (mut backend, slot, _listener, pointer) = connecting(false);
    let transfer = &backend.transfers[&slot];
    assert_eq!(transfer.request.body().as_ptr(), pointer);
    let head = transfer
        .connecting
        .as_ref()
        .expect("connecting")
        .cleartext_request
        .as_ref()
        .expect("cleartext head");
    assert!(
        head.capacity() < 1024,
        "TCP connect state retained a whole serialized body: {} bytes",
        head.capacity()
    );
    assert!(backend.reactor.outbound_is_empty(slot).expect("outbound"));
    backend.shutdown().expect("shutdown");
}

#[test]
fn m23_tls_handshake_does_not_duplicate_the_body() {
    let (mut backend, slot, _listener, pointer) = connecting(true);
    let transfer = &mut backend.transfers.get_mut(&slot).expect("transfer");
    assert_eq!(transfer.request.body().as_ptr(), pointer);
    let tls = transfer.tls.as_mut().expect("TLS");
    assert!(
        tls.request_plaintext_capacity() < 1024,
        "TLS retained a second whole request body before handshake"
    );
    let session = tls.take_handshake().expect("handshake session");
    assert!(tls.handshake_in_worker());
    assert!(tls.request_plaintext_capacity() < 1024);
    tls.restore_handshake(session);
    assert_eq!(transfer.request.body().as_ptr(), pointer);
    backend.shutdown().expect("shutdown");
}

#[test]
fn m23_cleartext_send_queue_has_a_body_independent_bound() {
    let (mut backend, slot, _listener, _) = connecting(false);
    let capacity = backend.reactor.outbound_capacity(slot).expect("capacity");
    assert!(
        capacity <= 65 * 1024,
        "large buffered bodies must use a bounded send window, got {capacity}"
    );
    backend.shutdown().expect("shutdown");
}

#[test]
fn m23_redirect_moves_the_original_body_allocation() {
    let mut bytes = Vec::with_capacity(LARGE_BODY + 37);
    bytes.resize(LARGE_BODY, 0x5a);
    let pointer = bytes.as_ptr();
    let request = Request::post("https://ownership.test/start")
        .header("Authorization", "secret")
        .header("Content-Length", LARGE_BODY.to_string())
        .body(bytes)
        .build()
        .expect("request");
    let redirected =
        request.redirected("https://other.test/finish".into(), Method::Post, true, true);
    assert_eq!(redirected.body().len(), LARGE_BODY);
    assert_eq!(
        redirected.body().as_ptr(),
        pointer,
        "body-preserving redirect copied the payload"
    );
    assert!(
        !redirected
            .headers()
            .iter()
            .any(|header| header.name().eq_ignore_ascii_case("authorization"))
    );
    assert!(
        redirected
            .headers()
            .iter()
            .any(|header| header.name().eq_ignore_ascii_case("content-length"))
    );
}

fn accept_bounded(listener: &TcpListener) -> std::net::TcpStream {
    listener.set_nonblocking(true).expect("bounded accept");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                socket.set_nonblocking(false).expect("blocking fixture I/O");
                socket
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .expect("read timeout");
                socket
                    .set_write_timeout(Some(Duration::from_secs(8)))
                    .expect("write timeout");
                return socket;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "fixture was not connected");
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("fixture accept: {error}"),
        }
    }
}

#[test]
fn m23_partial_buffered_send_cancel_discards_unsent_bytes() {
    let (mut backend, slot, listener, pointer) = connecting(false);
    let mut peer = accept_bounded(&listener);
    let deadline = Instant::now() + Duration::from_secs(5);
    while backend.transfers[&slot].request_body_offset == 0 {
        assert!(Instant::now() < deadline, "send never started");
        assert!(
            backend
                .poll(Instant::now() + Duration::from_millis(1))
                .expect("poll")
                .is_empty()
        );
    }
    let transfer = &backend.transfers[&slot];
    let id = transfer.request_id;
    assert_eq!(transfer.request.body().as_ptr(), pointer);
    assert!(transfer.request_body_offset < LARGE_BODY);
    assert!(!NativeHttpBackend::request_output_complete(transfer));
    assert!(!transfer.request_write_drained);
    backend.cancel(id);
    assert!(
        backend
            .process_events(vec![NativeEvent::WriteDrained(slot)])
            .expect("late event")
            .is_empty()
    );
    assert_eq!(backend.reactor.active_count(), 0);
    assert_eq!(backend.connection_count, 0);
    assert!(backend.transfers.is_empty());
    backend.shutdown().expect("shutdown");
    super::drain_until_socket_closed(&mut peer, "cancelled partial body");
}

fn serve_early_redirects(stream: &mut (impl Read + Write)) {
    for hop in 0..4 {
        // Read exactly the head: the redirect is deliberately returned before consuming the
        // body. Its response body waits for the upload, forcing the client to keep transmitting.
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).expect("request head");
            head.push(byte[0]);
            assert!(head.len() < 1024);
        }
        let head = String::from_utf8(head).expect("ASCII request head");
        let size = if hop == 3 { 50 * 1024 } else { LARGE_BODY };
        assert!(
            head.starts_with(&format!("POST /hop{hop} HTTP/1.1\r\n")),
            "{head}"
        );
        assert!(
            head.contains(&format!("Content-Length: {size}\r\n")),
            "{head}"
        );
        if hop < 2 {
            write!(
                stream,
                "HTTP/1.1 {} Redirect\r\nLocation: /hop{}\r\nContent-Length: 4\r\n\r\n",
                307 + hop,
                hop + 1
            )
            .expect("early redirect head");
            stream.flush().expect("early redirect flush");
        }
        let mut received = 0;
        let mut chunk = [0; 997];
        while received < size {
            let count = (size - received).min(chunk.len());
            stream
                .read_exact(&mut chunk[..count])
                .expect("complete upload body");
            for (index, byte) in chunk[..count].iter().enumerate() {
                assert_eq!(
                    *byte,
                    ((received + index) % 251) as u8,
                    "upload byte {} at hop {hop}",
                    received + index
                );
            }
            received += count;
        }
        match hop {
            0 | 1 => stream.write_all(b"skip").expect("redirect body"),
            2 => stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("final response"),
            _ => stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("small response"),
        }
        stream.flush().expect("response flush");
    }
}

#[test]
fn m23_large_bodies_survive_early_307_308_and_reuse_for_buffered_and_stream_readers() {
    for tls in [false, true] {
        for streaming in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let address = listener.local_addr().expect("address");
            let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(1);
            let mut factory = NativeHttpFactory::new(&config);
            let server_config = tls.then(|| {
                let key = rcgen::KeyPair::generate().expect("key");
                let cert = rcgen::CertificateParams::new(vec!["127.0.0.1".into()])
                    .expect("certificate parameters")
                    .self_signed(&key)
                    .expect("certificate");
                factory.tls =
                    Some(NativeTlsConfigs::with_test_root(cert.der().clone()).expect("client TLS"));
                let mut server = rustls::ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
                )
                .expect("server TLS");
                server.alpn_protocols = vec![b"http/1.1".to_vec()];
                Arc::new(server)
            });
            let server = std::thread::spawn(move || {
                let mut socket = accept_bounded(&listener);
                if let Some(config) = server_config {
                    let mut stream = rustls::StreamOwned::new(
                        rustls::ServerConnection::new(config).expect("TLS session"),
                        socket,
                    );
                    serve_early_redirects(&mut stream);
                    stream.conn.send_close_notify();
                    // The final Connection: close response is already flushed. The client may
                    // close first after receiving it; this best-effort TLS farewell is cleanup.
                    let _ = stream.flush();
                } else {
                    serve_early_redirects(&mut socket);
                }
            });
            let engine =
                crate::Engine::with_spawned_factory(config, Box::new(factory)).expect("Engine");
            let make_request = |hop, size| {
                Request::post(format!(
                    "{}://{address}/hop{hop}",
                    if tls { "https" } else { "http" }
                ))
                .body(
                    (0..size)
                        .map(|index| (index % 251) as u8)
                        .collect::<Vec<_>>(),
                )
                .redirect_limit(3)
                .total_timeout(Duration::from_secs(15))
                .build()
                .expect("request")
            };
            let request = make_request(0, LARGE_BODY);
            let first = if streaming {
                engine
                    .client()
                    .submit_stream(request.into())
                    .map_err(|error| error.to_string())
                    .and_then(|reader| reader.collect().map_err(|error| error.to_string()))
            } else {
                engine
                    .client()
                    .execute(request)
                    .map_err(|error| error.to_string())
            };
            let second = first
                .as_ref()
                .ok()
                .map(|_| engine.client().execute(make_request(3, 50 * 1024)));
            engine.shutdown().expect("joined shutdown");
            let served = server.join();
            assert_eq!(
                first.expect("redirected request").body(),
                b"ok",
                "tls={tls}, streaming={streaming}"
            );
            assert_eq!(
                second
                    .expect("small request issued")
                    .expect("small request succeeds")
                    .body(),
                b"ok"
            );
            served.expect("one-connection fixture must join");
        }
    }
}
