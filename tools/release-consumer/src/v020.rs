#[test]
fn shared_response_can_transfer_after_last_alias_drops() {
    let response = nbreq::Response::new(200, Vec::new(), b"owned bytes".to_vec());
    let alias = response.clone();
    let body = response.into_body().try_into_vec().expect_err("shared");
    drop(alias);
    assert_eq!(body.try_into_vec().unwrap(), b"owned bytes");
}

#[cfg(not(feature = "native"))]
#[test]
fn minimal_build_rejects_network_construction() {
    for config in [
        nbreq::EngineConfig::spawned(),
        nbreq::EngineConfig::manual(),
    ] {
        let error = nbreq::Engine::new(config).err().expect("no backend");
        assert_eq!(error.kind(), nbreq::ErrorKind::Unsupported);
    }
}

#[cfg(feature = "native")]
mod native {
    use crate::fixture::{Server, WAIT, read_head, reply};
    use nbreq::{
        Engine, ExecuteError, LimitKind, StreamRequest, TcpConnectRequest, TcpSendErrorKind,
        UploadBody,
    };
    use std::io::{Read, Write};

    #[test]
    fn bounded_http_tracks_retention_and_checks_spare_upload_capacity() {
        let server = Server::http(&[b'x'; 32 * 1024]);
        let engine = Engine::builder()
            .max_buffered_body_bytes(128 * 1024)
            .build()
            .unwrap();
        let response = engine
            .get(server.url())
            .max_response_body_bytes(64 * 1024)
            .total_timeout(WAIT)
            .call()
            .unwrap();
        assert_eq!(response.body().len(), 32 * 1024);
        let alias = response.clone();
        drop(response);
        assert!(engine.metrics().current().reserved_buffered_body_bytes() >= 32 * 1024);
        let mut spare = Vec::with_capacity(128 * 1024);
        spare.push(1);
        let error = engine.post("http://127.0.0.1:9/").send(spare).unwrap_err();
        match error {
            ExecuteError::Submission(error) => {
                assert_eq!(error.limit_kind(), Some(LimitKind::BufferedBodyBytes))
            }
            other => panic!("capacity must fail before network admission: {other}"),
        }
        let bytes = alias.into_body().try_into_vec().unwrap();
        assert_eq!(bytes.len(), 32 * 1024);
        engine.shutdown().unwrap();
        server.finish();
    }

    #[test]
    fn fixed_upload_and_streamed_response_use_public_owners() {
        let server = Server::new(|mut socket| {
            let head = read_head(&mut socket);
            assert!(head.starts_with("POST / "));
            assert!(head.to_ascii_lowercase().contains("content-length: 4\r\n"));
            let mut body = [0; 4];
            socket.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"ping");
            reply(&mut socket, b"pong");
        });
        let engine = Engine::builder().build().unwrap();
        let (body, mut sender) = UploadBody::fixed(4, 1024).unwrap();
        let request = StreamRequest::post(server.url())
            .body_stream(body)
            .total_timeout(WAIT)
            .build()
            .unwrap();
        let reader = engine.client().submit_stream(request).unwrap();
        sender.push(b"ping".to_vec()).unwrap();
        sender.finish().unwrap();
        let response = reader.collect().unwrap();
        assert_eq!(response.body(), b"pong");
        engine.shutdown().unwrap();
        server.finish();
    }

    #[test]
    fn tcp_refusal_preserves_input_and_half_close_keeps_reader() {
        let server = Server::new(|mut socket| {
            let mut bytes = Vec::new();
            (&mut socket).take(64).read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"hello");
            socket.write_all(&bytes).unwrap();
        });
        let engine = Engine::builder().build().unwrap();
        let request = TcpConnectRequest::literal(server.address)
            .connect_timeout(WAIT)
            .read_inactivity_timeout(WAIT)
            .write_inactivity_timeout(WAIT)
            .send_queue_bytes(8)
            .receive_queue_bytes(8)
            .build()
            .unwrap();
        let mut connection = engine.tcp_connector().execute(request).unwrap();
        let error = connection.try_send(vec![7; 9]).unwrap_err();
        assert_eq!(error.kind(), TcpSendErrorKind::ChunkTooLarge);
        assert_eq!(error.into_remaining(), vec![7; 9]);
        connection.send(b"hello".to_vec()).unwrap();
        connection.finish().unwrap();
        let mut all = Vec::new();
        let mut buffer = [0; 8];
        while let Some(count) = connection.read(&mut buffer).unwrap() {
            all.extend_from_slice(&buffer[..count]);
        }
        assert_eq!(all, b"hello");
        drop(connection);
        engine.shutdown().unwrap();
        server.finish();
    }

    #[test]
    fn manual_tcp_uses_passive_io_between_owner_drives() {
        use nbreq::{EngineBuilder, TcpConnectCompletion, TcpFinishStatus, TcpRead};
        use std::time::{Duration, Instant};
        let server = Server::new(|mut socket| {
            let mut bytes = Vec::new();
            (&mut socket).take(64).read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"manual");
            socket.write_all(&bytes).unwrap();
        });
        let mut engine = EngineBuilder::manual().build().unwrap();
        let pending = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::literal(server.address)
                    .connect_timeout(WAIT)
                    .build()
                    .unwrap(),
            )
            .unwrap();
        let mut connection = match engine.drive_until(pending).unwrap() {
            TcpConnectCompletion::Completed(connection) => connection,
            other => panic!("unexpected TCP completion: {other:?}"),
        };
        connection.try_send(b"manual".to_vec()).unwrap();
        let mut all = Vec::new();
        let mut buffer = [0; 16];
        let mut finished = false;
        let until = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < until, "manual TCP made no progress");
            if !finished {
                finished = matches!(connection.try_finish().unwrap(), TcpFinishStatus::Finished);
            }
            match connection.try_read(&mut buffer).unwrap() {
                TcpRead::Pending => {}
                TcpRead::Data(count) => all.extend_from_slice(&buffer[..count]),
                TcpRead::Eof => break,
                other => panic!("unexpected read: {other:?}"),
            }
            engine
                .drive(Instant::now() + Duration::from_millis(10))
                .unwrap();
        }
        assert!(finished);
        assert_eq!(all, b"manual");
        drop(connection);
        engine.shutdown().unwrap();
        server.finish();
    }
}

#[cfg(feature = "resolver")]
#[test]
fn public_resolver_configuration_is_available() {
    use nbreq::{AddressFamily, CacheMode, ResolveRequest};
    let request = ResolveRequest::hostname("example.com.")
        .address_family(AddressFamily::Ipv4)
        .cache_mode(CacheMode::Bypass)
        .use_search_suffixes(true)
        .build()
        .unwrap();
    assert!(request.is_absolute());
    assert!(!request.applies_search_suffixes());
    let _: fn(
        &mut nbreq::Engine,
        nbreq::PendingResolve,
    ) -> Result<nbreq::ResolveCompletion, nbreq::Error> =
        nbreq::Engine::drive_until::<nbreq::PendingResolve>;
}

#[cfg(all(feature = "native", feature = "resolver", feature = "test-support"))]
#[test]
fn injected_dns_uses_opt_in_test_support_with_real_udp() {
    use nbreq::{AddressFamily, EngineConfig, ResolveRequest, ResolveStatus};
    use std::net::UdpSocket;
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    server.set_read_timeout(Some(crate::fixture::WAIT)).unwrap();
    let address = server.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let mut bytes = [0_u8; 512];
        let (len, peer) = server.recv_from(&mut bytes).unwrap();
        assert!(len >= 17);
        let mut end = 12;
        while bytes[end] != 0 {
            end += usize::from(bytes[end]) + 1;
            assert!(end < len);
        }
        end += 5; // terminal label, QTYPE and QCLASS; omit any request OPT record.
        assert!(end <= len);
        let mut answer = bytes[..end].to_vec();
        answer[2..4].copy_from_slice(&[0x81, 0x80]);
        answer[6..8].copy_from_slice(&[0, 1]);
        answer[8..12].fill(0);
        answer.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 127, 0, 0, 1]);
        server.send_to(&answer, peer).unwrap();
    });
    let engine =
        nbreq::testing::native_http_engine_with_nameserver(EngineConfig::spawned(), address)
            .unwrap();
    let request = ResolveRequest::hostname("release.example")
        .address_family(AddressFamily::Ipv4)
        .total_timeout(crate::fixture::WAIT)
        .build()
        .unwrap();
    let answer = engine.resolver().execute(request).unwrap();
    assert_eq!(answer.status(), ResolveStatus::Answer);
    assert_eq!(
        answer.addresses()[0].address(),
        "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
    );
    engine.shutdown().unwrap();
    worker.join().unwrap();
}
