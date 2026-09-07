use super::*;

const LARGE: usize = 2 * 1024 * 1024;
const SMALL: usize = 50 * 1024;

fn accept(listener: &TcpListener) -> std::net::TcpStream {
    listener.set_nonblocking(true).expect("bounded accept");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).expect("blocking fixture");
                stream
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .expect("read bound");
                stream
                    .set_write_timeout(Some(Duration::from_secs(8)))
                    .expect("write bound");
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "accept deadline");
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("fixture accept: {error}"),
        }
    }
}

fn serve(stream: &mut (impl Read + Write)) {
    for size in [LARGE, SMALL] {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).expect("request head");
            head.push(byte[0]);
            assert!(head.len() < 4096);
        }
        assert!(
            String::from_utf8(head)
                .expect("head ASCII")
                .contains(&format!("Content-Length: {size}\r\n"))
        );
        let mut offset = 0;
        let mut chunk = [0; 997];
        while offset < size {
            let count = chunk.len().min(size - offset);
            stream
                .read_exact(&mut chunk[..count])
                .expect("request body");
            for (index, value) in chunk[..count].iter().enumerate() {
                assert_eq!(*value, ((offset + index) % 251) as u8);
            }
            offset += count;
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx")
            .expect("response");
        stream.flush().expect("response flush");
    }
    // Keep the connection available until the test explicitly shuts down the backend.
    let mut byte = [0];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            ) => {}
        other => panic!("expected joined client close: {other:?}"),
    }
}

fn exercise_idle_send_storage(streaming: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = rcgen::CertificateParams::new(vec!["127.0.0.1".into()])
        .expect("params")
        .self_signed(&key)
        .expect("cert");
    let configs = NativeTlsConfigs::with_test_root(cert.der().clone()).expect("client trust");
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
    .expect("identity");
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let server = thread::spawn(move || {
        let stream = accept(&listener);
        let session = rustls::ServerConnection::new(Arc::new(server_config)).expect("session");
        serve(&mut rustls::StreamOwned::new(session, stream));
    });
    let config = EngineConfig::manual();
    let mut backend = NativeHttpBackend::new(
        HttpLimits::from_config(&config),
        None,
        Some(configs),
        ConnectionLimits::from_config(&config),
    )
    .expect("backend");
    let (owner, _controller) = crate::testing::engine(config).expect("synthetic handle owner");
    let mut capacities = Vec::new();
    let mut slots = Vec::new();
    for (index, size) in [LARGE, SMALL].into_iter().enumerate() {
        let pending = owner
            .client()
            .submit(
                Request::get("http://handle.invalid/")
                    .build()
                    .expect("handle request"),
            )
            .expect("handle");
        let id = pending.request_id();
        let request = Request::post(format!("https://{address}/body"))
            .body((0..size).map(|i| (i % 251) as u8).collect::<Vec<_>>())
            .total_timeout(Duration::from_secs(8))
            .build()
            .expect("request");
        let mut reader = if streaming {
            let (reader, sink, _) = crate::stream::response_pair(
                pending.handle(),
                crate::RunMode::Manual,
                1,
                1024,
                None,
                None,
            )
            .expect("response pair");
            backend.submit_stream(id, request.into(), sink, Instant::now());
            Some(reader)
        } else {
            assert!(backend.submit(id, request, Instant::now()).is_none());
            None
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut reply = Vec::new();
        let mut complete = false;
        while !complete || backend.idle_count == 0 {
            assert!(
                Instant::now() < deadline,
                "response deadline at round {index}"
            );
            let completions = backend
                .poll(Instant::now() + Duration::from_millis(1))
                .expect("poll");
            for completion in completions {
                assert!(!streaming);
                let Completion::Completed(response) = completion.completion else {
                    panic!("failed: {:?}", completion.completion)
                };
                reply.extend_from_slice(response.body());
                complete = true;
            }
            if let Some(reader) = &mut reader {
                let mut byte = [0];
                match reader.try_read(&mut byte).expect("stream read") {
                    crate::StreamRead::Data(count) => reply.extend_from_slice(&byte[..count]),
                    crate::StreamRead::Eof => complete = true,
                    crate::StreamRead::Pending => {}
                }
            }
        }
        assert_eq!(reply, b"x");
        let slot = *backend.idle_slots.keys().next().expect("idle connection");
        capacities.push(backend.reactor.outbound_storage_capacity(slot));
        slots.push(slot);
        drop(pending);
    }
    backend.shutdown().expect("backend shutdown");
    owner.shutdown().expect("synthetic owner shutdown");
    server.join().expect("fixture joined");
    assert_eq!(
        slots[0], slots[1],
        "large-to-small must reuse the same connection"
    );
    assert!(
        capacities.iter().all(|capacity| *capacity <= 128 * 1024),
        "streaming={streaming}: idle send queues retain oversized allocations: {capacities:?}"
    );
}

#[test]
fn m24_buffered_tls_parking_bounds_retained_send_storage() {
    exercise_idle_send_storage(false);
}

#[test]
fn m24_streaming_tls_parking_bounds_retained_send_storage() {
    exercise_idle_send_storage(true);
}
