use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use crate::{Engine, ExecuteError, LimitKind, Request, StreamRequest, UploadBody};

#[test]
fn m3_https_budget_covers_plaintext_and_failed_exchange_releases_every_charge() {
    use std::sync::Arc;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = rcgen::CertificateParams::new(vec!["127.0.0.1".into()])
        .expect("parameters")
        .self_signed(&key)
        .expect("certificate");
    let mut server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocols")
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
    .expect("server config");
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let engine = Engine::new(
        crate::EngineConfig::spawned()
            .with_additional_tls_root_certificate(cert.der().to_vec())
            .with_max_buffered_body_bytes(48 * 1024),
    )
    .expect("engine");
    let ledger = engine.client().shared.body_budget.clone();
    let server = thread::spawn(move || {
        let (socket, _) = listener.accept().expect("accept");
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("timeout");
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .expect("timeout");
        let session = rustls::ServerConnection::new(Arc::new(server_config)).expect("TLS");
        let mut stream = rustls::StreamOwned::new(session, socket);
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).expect("head");
            head.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n20000\r\n")
            .expect("head");
        let _ = stream.write_all(&vec![b'x'; 128 * 1024]);
    });
    let result = engine
        .get(format!("https://{address}/"))
        .total_timeout(Duration::from_secs(3))
        .call();
    server.join().expect("server");
    engine.shutdown().expect("shutdown");
    let Err(ExecuteError::Failed(error)) = result else {
        panic!("expected exhaustion")
    };
    assert_eq!(error.limit_kind(), Some(LimitKind::BufferedBodyBytes));
    assert_eq!(ledger.used(), 0);
    assert!(ledger.peak() <= 48 * 1024);
}

#[test]
fn m3_streaming_receive_window_is_separate_but_buffered_upload_is_charged() {
    let engine = Engine::builder()
        .max_buffered_body_bytes(3)
        .build()
        .expect("engine");
    let request = StreamRequest::post("http://127.0.0.1:9/")
        .body(b"four".to_vec())
        .build()
        .expect("request");
    let error = engine
        .client()
        .submit_stream(request)
        .expect_err("buffered upload must be admitted");
    assert_eq!(error.limit_kind(), Some(LimitKind::BufferedBodyBytes));
    let (url, server) = reply(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nfour");
    let request = StreamRequest::get(url)
        .max_response_body_bytes(4)
        .build()
        .expect("request");
    let response = engine
        .client()
        .submit_stream(request)
        .expect("stream")
        .collect()
        .expect("receive uses stream queue budget");
    assert_eq!(response.body(), b"four");
    server.join().expect("server");
    let ledger = engine.client().shared.body_budget.clone();
    engine.shutdown().expect("shutdown");
    assert_eq!(ledger.used(), 0);
}

#[test]
fn m3_queued_calls_reserve_actual_upload_capacity_without_whole_reply_allowances() {
    let (engine, _) =
        crate::testing::engine(crate::EngineConfig::manual().with_max_buffered_body_bytes(16))
            .expect("engine");
    let client = engine.client();
    let ledger = client.shared.body_budget.clone();
    let mut pending = Vec::new();
    for _ in 0..8 {
        pending.push(
            client
                .submit(
                    Request::get("http://example.invalid/")
                        .build()
                        .expect("request"),
                )
                .expect("quiet call"),
        );
    }
    assert_eq!(ledger.used(), 0);
    for _ in 0..2 {
        pending.push(
            client
                .submit(
                    Request::post("http://example.invalid/")
                        .body(Vec::with_capacity(8))
                        .build()
                        .expect("request"),
                )
                .expect("queued upload"),
        );
    }
    assert_eq!(ledger.used(), 16);
    let error = client
        .submit(
            Request::post("http://example.invalid/")
                .body(vec![1])
                .build()
                .expect("request"),
        )
        .expect_err("full");
    assert_eq!(error.limit_kind(), Some(LimitKind::BufferedBodyBytes));
    engine.shutdown().expect("shutdown");
    assert_eq!(ledger.used(), 0);
    drop(pending);
}

#[test]
fn m3_aggregate_shared_results_survive_shutdown_and_unique_transfer_refunds() {
    let (url, server) = reply(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc");
    let engine = Engine::builder()
        .max_buffered_body_bytes(64 * 1024)
        .build()
        .expect("engine");
    let response = engine.get(url).call().expect("response");
    server.join().expect("server");
    let ledger = engine.client().shared.body_budget.clone();
    engine.shutdown().expect("shutdown");
    let used = || ledger.used();
    assert_eq!(used(), 3);
    let clone = response.clone();
    let pointer = response.body().as_ptr();
    let body = response
        .into_body()
        .try_into_vec()
        .expect_err("shared storage");
    assert_eq!(used(), 3);
    drop(clone);
    let bytes = body.try_into_vec().expect("unique storage");
    assert_eq!(bytes.as_ptr(), pointer);
    assert_eq!(bytes, b"abc");
    assert_eq!(used(), 0);
}

#[test]
fn m3_aggregate_unknown_reply_exhaustion_does_not_retry_post_and_later_work_progresses() {
    for framing in [
        b"Transfer-Encoding: chunked\r\n\r\n20000\r\n".as_slice(),
        b"Connection: close\r\n\r\n".as_slice(),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/", listener.local_addr().expect("address"));
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept POST");
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("timeout");
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).expect("head");
                head.push(byte[0]);
            }
            assert!(head.starts_with(b"POST "));
            socket.write_all(b"HTTP/1.1 200 OK\r\n").expect("head");
            socket.write_all(framing).expect("framing");
            // A response-side failure follows the server accepting the POST. Do not replay it.
            let _ = socket.write_all(&vec![b'x'; 128 * 1024]);
            drop(socket);
            listener.set_nonblocking(true).expect("nonblocking");
            assert!(
                matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
            );
        });
        let engine = Engine::builder()
            .max_buffered_body_bytes(48 * 1024)
            .build()
            .expect("engine");
        let result = engine
            .post(url)
            .total_timeout(Duration::from_secs(3))
            .send_empty();
        server.join().expect("server");
        let Err(ExecuteError::Failed(error)) = result else {
            panic!("expected exhaustion")
        };
        assert_eq!(error.limit_kind(), Some(LimitKind::BufferedBodyBytes));
        let (url, server) = reply(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let response = engine
            .get(url)
            .total_timeout(Duration::from_secs(3))
            .call()
            .expect("progress after exhaustion");
        assert_eq!(response.body(), b"ok");
        drop(response);
        server.join().expect("server");
        let ledger = engine.client().shared.body_budget.clone();
        engine.shutdown().expect("shutdown");
        assert_eq!(ledger.used(), 0);
        assert!(ledger.peak() <= 48 * 1024);
    }
}

#[test]
fn m3_aggregate_admission_counts_spare_request_capacity() {
    let engine = Engine::builder()
        .max_buffered_body_bytes(1024)
        .build()
        .expect("engine");
    let mut bytes = Vec::with_capacity(2048);
    bytes.push(1);
    let request = Request::post("http://127.0.0.1:9/")
        .body(bytes)
        .build()
        .expect("request");
    let error = engine
        .client()
        .submit(request)
        .expect_err("capacity must reject before admission");
    assert_eq!(error.limit_kind(), Some(LimitKind::BufferedBodyBytes));
    assert_eq!(engine.metrics().requests_accepted(), 0);
    engine.shutdown().expect("shutdown");
}

#[test]
fn m3_aggregate_known_length_rejected_before_missing_body() {
    let (url, server) = reply(b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\n\r\n");
    let engine = Engine::builder()
        .max_buffered_body_bytes(128 * 1024)
        .build()
        .expect("engine");
    let result = engine.get(url).total_timeout(Duration::from_secs(3)).call();
    server.join().expect("server");
    engine.shutdown().expect("shutdown");
    let Err(ExecuteError::Failed(error)) = result else {
        panic!("expected failure: {result:?}")
    };
    assert_eq!(error.limit_kind(), Some(LimitKind::BufferedBodyBytes));
}

// Closing after the head is deliberate: an early size rejection must beat a truncated-body
// transport error, without depending on server timing or allocating the advertised body.
fn reply(bytes: &'static [u8]) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("timeout");
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).expect("request head");
            head.push(byte[0]);
        }
        socket.write_all(bytes).expect("response");
    });
    (url, server)
}

#[test]
fn m3_per_request_upload_rejected_before_admission() {
    let engine = Engine::builder().build().expect("engine");
    let request = Request::post("http://127.0.0.1:9/")
        .body(b"four".to_vec())
        .max_request_body_bytes(3)
        .build()
        .expect("request");
    let error = engine
        .client()
        .submit(request)
        .expect_err("must reject before admission");
    assert_eq!(error.limit_kind(), Some(LimitKind::RequestBodyBytes));
    assert_eq!(engine.metrics().requests_accepted(), 0);
    engine.shutdown().expect("shutdown");
}

#[test]
fn m3_per_request_fixed_and_chunked_limits_reject_at_framing() {
    for wire in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n".as_slice(),
    ] {
        let (url, server) = reply(wire);
        let engine = Engine::builder().build().expect("engine");
        let result = engine
            .get(url)
            .max_response_body_bytes(3)
            .total_timeout(Duration::from_secs(3))
            .call();
        server.join().expect("server");
        engine.shutdown().expect("shutdown");
        let Err(ExecuteError::Failed(error)) = result else {
            panic!("expected failure: {result:?}")
        };
        assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
    }
}

#[test]
fn m3_per_request_stream_upload_checks_declared_length_before_admission() {
    let engine = Engine::builder().build().expect("engine");
    let (upload, _sender) = UploadBody::fixed(4, 8).expect("upload");
    let request = StreamRequest::post("http://127.0.0.1:9/")
        .body_stream(upload)
        .max_request_body_bytes(3)
        .build()
        .expect("request");
    let error = engine
        .client()
        .submit_stream(request)
        .expect_err("must reject");
    assert_eq!(error.limit_kind(), Some(LimitKind::RequestBodyBytes));
    assert_eq!(engine.metrics().requests_accepted(), 0);
    engine.shutdown().expect("shutdown");
}

#[test]
fn m3_per_request_limits_cannot_raise_engine_ceiling_and_head_needs_no_body() {
    for (method, wire, request_limit, succeeds) in [
        (
            crate::Method::Get,
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nfour".as_slice(),
            10,
            false,
        ),
        (
            crate::Method::Get,
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".as_slice(),
            3,
            true,
        ),
        (
            crate::Method::Head,
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n".as_slice(),
            0,
            true,
        ),
    ] {
        let (url, server) = reply(wire);
        let engine = Engine::builder()
            .max_response_body_bytes(3)
            .build()
            .expect("engine");
        let request = Request::builder(method, url)
            .max_response_body_bytes(request_limit)
            .total_timeout(Duration::from_secs(3))
            .build()
            .expect("request");
        let result = engine.client().execute(request);
        server.join().expect("server");
        engine.shutdown().expect("shutdown");
        if succeeds {
            assert!(result.is_ok(), "{result:?}");
        } else {
            let Err(ExecuteError::Failed(error)) = result else {
                panic!("expected failure")
            };
            assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
        }
    }
}
