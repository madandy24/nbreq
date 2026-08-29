use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::{
    Completion, Engine, EngineConfig, ErrorKind, ExecuteError, LimitKind, Request, TimeoutKind,
    TransportStage, TryPushErrorKind, UploadBody,
};

const LIMITS: HttpLimits = HttpLimits {
    body_bytes: 1024,
    header_bytes: 1024,
    header_count: 16,
};

#[test]
fn checked_in_fuzz_seeds_satisfy_the_fragmentation_oracle() {
    for seed in [
        include_bytes!("../../../fuzz/corpus/native_response_decoder/fixed.seed").as_slice(),
        include_bytes!("../../../fuzz/corpus/native_response_decoder/chunked.seed").as_slice(),
        include_bytes!("../../../fuzz/corpus/native_response_decoder/informational.seed")
            .as_slice(),
        include_bytes!("../../../fuzz/corpus/native_response_decoder/close_delimited.seed")
            .as_slice(),
        include_bytes!("../../../fuzz/corpus/native_response_decoder/conflicting_length.seed")
            .as_slice(),
        include_bytes!("../../../fuzz/corpus/native_response_decoder/malformed_chunk.seed")
            .as_slice(),
    ] {
        fuzz_response_decoder(seed);
    }
}

#[test]
fn checked_in_streaming_fuzz_seeds_cross_reader_backpressure() {
    for seed in [
        include_bytes!(
            "../../../fuzz/corpus/native_streaming_response_decoder/fixed_tiny_queue.seed"
        )
        .as_slice(),
        include_bytes!(
            "../../../fuzz/corpus/native_streaming_response_decoder/chunked_tiny_queue.seed"
        )
        .as_slice(),
        include_bytes!("../../../fuzz/corpus/native_streaming_response_decoder/no_body.seed")
            .as_slice(),
        include_bytes!(
            "../../../fuzz/corpus/native_streaming_response_decoder/close_delimited.seed"
        )
        .as_slice(),
        include_bytes!(
            "../../../fuzz/corpus/native_streaming_response_decoder/discard_redirect.seed"
        )
        .as_slice(),
    ] {
        let (engine, _controller) = crate::testing::engine(EngineConfig::manual())
            .expect("streaming fuzz seed Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get("http://fuzz.invalid/")
                    .build()
                    .expect("streaming fuzz seed request must build"),
            )
            .expect("streaming fuzz seed request must submit");
        let handle = pending.handle();
        drop(pending);
        fuzz_streaming_response_decoder(seed, handle);
        engine.cancel_all();
        engine
            .shutdown()
            .expect("streaming fuzz seed Engine must stop");
    }
}

fn synthetic_stream_decoder(
    queue_capacity: usize,
) -> (Engine, crate::ResponseReader, StreamingResponseDecoder) {
    let (engine, _controller) =
        crate::testing::engine(EngineConfig::spawned()).expect("held Engine must construct");
    let pending = engine
        .client()
        .submit(
            Request::get("http://example.test/")
                .build()
                .expect("synthetic handle request must build"),
        )
        .expect("synthetic handle request must submit");
    let handle = pending.handle();
    drop(pending);
    let (reader, sink, _control) = crate::stream::response_pair(
        handle,
        crate::RunMode::Spawned,
        queue_capacity,
        LIMITS.body_bytes,
        None,
        None,
    )
    .expect("synthetic response pair must construct");
    (
        engine,
        reader,
        StreamingResponseDecoder::new(false, LIMITS, sink),
    )
}

#[test]
fn streaming_decoder_publishes_head_then_body_and_reports_exact_boundary() {
    let (engine, reader, mut decoder) = synthetic_stream_decoder(8);
    let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Test: yes\r\n\r\nhelloextra";
    let head_consumed = match decoder.ingest(wire).expect("head must parse") {
        StreamDecodeProgress::Head { head, consumed } => {
            assert_eq!(head.status(), 200);
            assert!(head.headers().iter().any(|header| {
                header.name().eq_ignore_ascii_case("x-test") && header.value() == b"yes"
            }));
            consumed
        }
        other => panic!("stream decoder skipped the head boundary: {other:?}"),
    };
    let decision = decoder.decide_head(true).expect("head must publish");
    assert!(!decision.complete);
    let body_consumed = match decoder
        .ingest(&wire[head_consumed..])
        .expect("body must decode")
    {
        StreamDecodeProgress::Complete {
            consumed,
            delivered,
            ..
        } => {
            assert!(delivered);
            consumed
        }
        other => panic!("stream body did not complete: {other:?}"),
    };
    assert_eq!(body_consumed, 5, "trailing bytes must remain unconsumed");
    let response = reader
        .collect()
        .expect("reader must collect delivered body");
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"hello");
    engine.cancel_all();
    engine.shutdown().expect("held Engine must stop");
}

#[test]
fn streaming_decoder_splits_body_to_the_current_reader_hole() {
    let (engine, mut reader, mut decoder) = synthetic_stream_decoder(3);
    let head = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n";
    assert!(matches!(
        decoder.ingest(head).expect("head must parse"),
        StreamDecodeProgress::Head { consumed, .. } if consumed == head.len()
    ));
    decoder.decide_head(true).expect("head must publish");

    let body = b"abcdefgh";
    let mut offset = 0;
    let mut received = Vec::new();
    while offset < body.len() {
        match decoder
            .ingest(&body[offset..])
            .expect("bounded body pass must decode")
        {
            StreamDecodeProgress::Body { consumed, blocked } => {
                assert!(blocked);
                assert_ne!(consumed, 0);
                offset += consumed;
            }
            StreamDecodeProgress::Complete { consumed, .. } => {
                offset += consumed;
            }
            StreamDecodeProgress::Head { .. } => panic!("head was published twice"),
        }
        let mut hole = [0_u8; 2];
        if let Some(count) = reader
            .read(&mut hole)
            .expect("reader must drain a small hole")
        {
            received.extend_from_slice(&hole[..count]);
        }
    }
    let mut tail = [0_u8; 8];
    while let Some(count) = reader.read(&mut tail).expect("reader must reach EOF") {
        received.extend_from_slice(&tail[..count]);
    }
    assert_eq!(received, body);
    engine.cancel_all();
    engine.shutdown().expect("held Engine must stop");
}

#[test]
fn streaming_decoder_no_body_head_is_complete_and_redirect_discard_reuses_sink() {
    let (engine, mut reader, mut decoder) = synthetic_stream_decoder(8);
    let no_body = b"HTTP/1.1 204 No Content\r\n\r\n";
    assert!(matches!(
        decoder.ingest(no_body).expect("204 head must parse"),
        StreamDecodeProgress::Head { .. }
    ));
    let decision = decoder.decide_head(true).expect("204 head must publish");
    assert!(decision.complete);
    assert_eq!(
        reader.wait_head().expect("204 head must arrive").status(),
        204
    );
    assert!(reader.is_eof());
    drop(reader);
    engine.cancel_all();
    engine.shutdown().expect("held Engine must stop");

    let (engine, reader, mut redirect) = synthetic_stream_decoder(8);
    let first = b"HTTP/1.1 302 Found\r\nContent-Length: 3\r\nLocation: /next\r\n\r\nold";
    let head_used = match redirect.ingest(first).expect("redirect head must parse") {
        StreamDecodeProgress::Head { consumed, .. } => consumed,
        other => panic!("redirect head was not exposed for policy: {other:?}"),
    };
    redirect
        .decide_head(false)
        .expect("redirect head must remain private");
    assert!(matches!(
        redirect
            .ingest(&first[head_used..])
            .expect("redirect body must drain"),
        StreamDecodeProgress::Complete {
            delivered: false,
            ..
        }
    ));
    let sink = redirect
        .into_response()
        .expect("discarded redirect must return the unique sink");
    let mut final_decoder = StreamingResponseDecoder::new(false, LIMITS, sink);
    let final_wire = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew";
    let head_used = match final_decoder
        .ingest(final_wire)
        .expect("final head must parse")
    {
        StreamDecodeProgress::Head { consumed, .. } => consumed,
        other => panic!("final head was not exposed: {other:?}"),
    };
    final_decoder
        .decide_head(true)
        .expect("final head must publish");
    assert!(matches!(
        final_decoder
            .ingest(&final_wire[head_used..])
            .expect("final body must decode"),
        StreamDecodeProgress::Complete {
            delivered: true,
            ..
        }
    ));
    let response = reader.collect().expect("reader sees only the final hop");
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"new");
    engine.cancel_all();
    engine.shutdown().expect("held Engine must stop");
}

#[test]
fn streaming_close_delimited_body_drains_retained_bytes_before_fin_completes_it() {
    let (engine, mut reader, mut decoder) = synthetic_stream_decoder(3);
    let head = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
    assert!(matches!(
        decoder.ingest(head).expect("close-delimited head must parse"),
        StreamDecodeProgress::Head { consumed, .. } if consumed == head.len()
    ));
    decoder
        .decide_head(true)
        .expect("close-delimited head must publish");

    let body = b"abcde";
    let consumed = match decoder
        .ingest(body)
        .expect("first close-delimited window must decode")
    {
        StreamDecodeProgress::Body { consumed, blocked } => {
            assert!(blocked);
            consumed
        }
        other => panic!("close-delimited body did not pause: {other:?}"),
    };
    assert_eq!(consumed, 3);
    let mut first = [0_u8; 3];
    assert_eq!(reader.read(&mut first).expect("reader must drain"), Some(3));
    assert_eq!(&first, b"abc");

    assert!(matches!(
        decoder
            .ingest(&body[consumed..])
            .expect("retained close-delimited bytes must drain"),
        StreamDecodeProgress::Body {
            consumed: 2,
            blocked: false
        }
    ));
    assert_eq!(
        decoder.eof().expect("peer FIN must complete body"),
        Some((false, true))
    );
    let mut tail = [0_u8; 3];
    assert_eq!(reader.read(&mut tail).expect("tail must drain"), Some(2));
    assert_eq!(&tail[..2], b"de");
    assert_eq!(reader.read(&mut tail).expect("reader must reach EOF"), None);
    engine.cancel_all();
    engine.shutdown().expect("held Engine must stop");
}

#[test]
fn streaming_decoder_preserves_informational_chunked_trailer_and_limit_rules() {
    let (engine, mut reader, mut decoder) = synthetic_stream_decoder(4);
    let wire = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nX-Trailer: yes\r\n\r\n";
    let head_used = match decoder.ingest(wire).expect("final head must parse") {
        StreamDecodeProgress::Head { head, consumed } => {
            assert_eq!(head.status(), 200);
            consumed
        }
        other => panic!("informational head escaped or final head vanished: {other:?}"),
    };
    decoder.decide_head(true).expect("final head must publish");
    let mut offset = head_used;
    let mut received = Vec::new();
    while offset < wire.len() {
        match decoder
            .ingest(&wire[offset..])
            .expect("chunked framing must decode")
        {
            StreamDecodeProgress::Body { consumed, blocked } => {
                assert!(blocked || offset + consumed == wire.len());
                assert_ne!(consumed, 0);
                offset += consumed;
            }
            StreamDecodeProgress::Complete { consumed, .. } => {
                offset += consumed;
                break;
            }
            StreamDecodeProgress::Head { .. } => panic!("final head was published twice"),
        }
        let mut hole = [0_u8; 3];
        if let Some(count) = reader.read(&mut hole).expect("chunked reader must drain") {
            received.extend_from_slice(&hole[..count]);
        }
    }
    assert_eq!(offset, wire.len());
    let mut tail = [0_u8; 8];
    while let Some(count) = reader.read(&mut tail).expect("chunked reader must finish") {
        received.extend_from_slice(&tail[..count]);
    }
    assert_eq!(received, b"hello");
    engine.cancel_all();
    engine.shutdown().expect("held Engine must stop");

    let (engine, mut reader, mut decoder) = synthetic_stream_decoder(8);
    decoder.limits.body_bytes = 4;
    let error = decoder
        .ingest(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")
        .expect_err("oversize final head must fail before publication");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
    decoder.fail(error.clone());
    assert!(matches!(
        reader.try_head(),
        Err(crate::StreamError::Failed(observed)) if observed == error
    ));
    engine.cancel_all();
    engine.shutdown().expect("held Engine must stop");
}

fn read_request_head(stream: &mut std::net::TcpStream, label: &str) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 512];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("{label} must read: {error}"));
        assert_ne!(read, 0, "client closed before {label}");
        request.extend_from_slice(&buffer[..read]);
    }
}

fn read_request_wire(stream: &mut std::net::TcpStream, label: &str) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 512];
    let head_end = loop {
        if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("{label} must read: {error}"));
        assert_ne!(read, 0, "client closed before {label}");
        request.extend_from_slice(&buffer[..read]);
    };
    let content_length = std::str::from_utf8(&request[..head_end])
        .expect("test request head must be UTF-8")
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().expect("test Content-Length"))
            })
        })
        .unwrap_or(0);
    let total = head_end + content_length;
    while request.len() < total {
        let read = stream
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("{label} body must read: {error}"));
        assert_ne!(read, 0, "client closed before {label} body");
        request.extend_from_slice(&buffer[..read]);
    }
    request.truncate(total);
    request
}

fn push_eventually(sender: &mut crate::UploadSender, mut chunk: Vec<u8>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match sender.try_push(chunk) {
            Ok(()) => return,
            Err(error) if error.kind() == TryPushErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "{label} remained backpressured");
                chunk = error.into_chunk();
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("{label} failed unexpectedly: {error}"),
        }
    }
}

fn peer_observed_close(stream: &mut std::net::TcpStream) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("peer close timeout must configure");
    let mut buffer = [0_u8; 64];
    match stream.read(&mut buffer) {
        Ok(0) => true,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            true
        }
        Ok(_) | Err(_) => false,
    }
}

#[test]
fn terminal_socket_failure_dominates_same_batch_write_progress() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("batch fixture must bind");
    let address = listener.local_addr().expect("batch fixture address");
    let config = EngineConfig::manual();
    let mut backend =
        NativeHttpBackend::new(LIMITS, None, None, ConnectionLimits::from_config(&config))
            .expect("native backend must construct");
    let deadline = Instant::now() + Duration::from_secs(5);
    let slot = backend
        .reactor
        .connect(address, Some(deadline), 1024, 1024)
        .expect("batch fixture must connect");
    let (_peer, _) = listener.accept().expect("batch fixture must accept");
    let request_id = RequestId {
        engine: 1,
        sequence: 1,
    };
    backend.request_to_slot.insert(request_id, slot);
    let connection_key = ConnectionKey {
        scheme: "http".to_owned(),
        host: "127.0.0.1".to_owned(),
        port: address.port(),
        dangerously_disable_tls_verification: false,
    };
    assert!(backend.reserve_connection(&connection_key));
    backend.transfers.insert(
        slot,
        HttpTransfer {
            request_id,
            response: TransferResponse::Buffered(ResponseDecoder::new(false, LIMITS)),
            body_bearing: true,
            response_started: false,
            connected: true,
            tls: None,
            connect_deadline: None,
            total_deadline: Some(deadline),
            inactivity_timeout: Some(Duration::from_secs(1)),
            inactivity_deadline: Some(deadline),
            inactivity_paused: false,
            key: connection_key,
            request_permits_reuse: true,
            request_write_drained: false,
            request: Request::get(format!("http://{address}/batch"))
                .build()
                .expect("batch request must build"),
            redirect_hops: 0,
            upload: None,
            upload_aborted: false,
        },
    );

    assert!(backend.reactor.cancel(slot));
    let completions = backend
        .process_events(vec![
            NativeEvent::WriteProgress(slot, 1),
            NativeEvent::Failed(
                slot,
                NativeFailure {
                    kind: NativeFailureKind::Read,
                    message: "simulated same-batch reset".to_owned(),
                    io_kind: None,
                },
            ),
        ])
        .expect("same-batch progress must not re-arm a removed socket");

    assert_eq!(completions.len(), 1);
    let Completion::Failed(error) = &completions[0].completion else {
        panic!("same-batch reset must fail the request");
    };
    assert_eq!(error.kind(), ErrorKind::Transport);
    assert_eq!(error.transport_stage(), Some(TransportStage::Send));
}

#[test]
fn standalone_terminal_socket_failure_dominates_same_batch_write_progress() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("TCP batch fixture must bind");
    let address = listener.local_addr().expect("TCP batch fixture address");
    let config = EngineConfig::manual();
    let mut backend =
        NativeHttpBackend::new(LIMITS, None, None, ConnectionLimits::from_config(&config))
            .expect("native backend must construct");
    let slot = backend
        .reactor
        .connect(address, None, 1024, 1024)
        .expect("TCP batch fixture must connect");
    let (_peer, _) = listener.accept().expect("TCP batch fixture must accept");
    let engine = crate::EngineBuilder::manual()
        .build()
        .expect("manual Engine must construct");
    let shared = engine.shared_for_testing();
    let request_id = RequestId {
        engine: shared.id,
        sequence: 1,
    };
    let (_io, owner) = crate::tcp::io::TcpIoShared::pair(crate::tcp::io::TcpIoConfig {
        engine_id: shared.id,
        request_id,
        shared,
        run_mode: crate::RunMode::Manual,
        send_window: 1024,
        receive_window: 1024,
        local: address,
        peer: address,
        engine_waker: Some(Arc::new(|| {})),
        on_release: Box::new(|| {}),
    });
    backend.standalone_request_to_slot.insert(request_id, slot);
    backend.standalone_live.insert(
        slot,
        StandaloneTcp::new(
            request_id,
            owner,
            Some(Duration::from_secs(1)),
            Some(Duration::from_secs(1)),
            Instant::now(),
        ),
    );

    assert!(backend.reactor.cancel(slot));
    backend
        .process_events(vec![
            NativeEvent::WriteProgress(slot, 1),
            NativeEvent::Failed(
                slot,
                NativeFailure {
                    kind: NativeFailureKind::Read,
                    message: "simulated standalone same-batch reset".to_owned(),
                    io_kind: Some(std::io::ErrorKind::ConnectionReset),
                },
            ),
        ])
        .expect("same-batch standalone progress must not re-arm a removed socket");

    assert!(!backend.standalone_live.contains_key(&slot));
    assert!(!backend.standalone_request_to_slot.contains_key(&request_id));
}

fn assert_socket_closed(stream: &mut std::net::TcpStream, buffer: &mut [u8], context: &str) {
    match stream.read(buffer) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) => {}
        Ok(read) => panic!("{context}: received {read} bytes instead of socket close"),
        Err(error) => panic!("{context}: did not observe socket close: {error}"),
    }
}

fn drain_until_socket_closed(stream: &mut std::net::TcpStream, context: &str) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("socket-close timeout must configure");
    let mut buffer = [0_u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) =>
            {
                return;
            }
            Err(error) => panic!("{context}: did not observe socket close: {error}"),
        }
    }
}

fn decode_fragmented(response_to_head: bool, bytes: &[u8], eof: bool) -> Result<Response, Error> {
    let mut decoder = ResponseDecoder::new(response_to_head, LIMITS);
    for byte in bytes {
        if let Some(response) = decoder.ingest(std::slice::from_ref(byte))?.response {
            return Ok(response);
        }
    }
    if eof {
        if let Some(response) = decoder.eof()? {
            return Ok(response);
        }
    }
    Err(Error::new(
        ErrorKind::Internal,
        "test response did not reach terminal framing",
    ))
}

fn decode_at_split(bytes: &[u8], split: usize, eof: bool) -> Result<Response, Error> {
    let mut decoder = ResponseDecoder::new(false, LIMITS);
    for part in [&bytes[..split], &bytes[split..]] {
        if let Some(response) = decoder.ingest(part)?.response {
            return Ok(response);
        }
    }
    if eof {
        if let Some(response) = decoder.eof()? {
            return Ok(response);
        }
    }
    Err(Error::new(
        ErrorKind::Internal,
        "split test response did not reach terminal framing",
    ))
}

#[test]
fn serializes_origin_form_host_lengths_and_binary_headers() {
    let request = Request::post("http://example.test:8080/path?q=yes#ignored")
        .header("X-Binary", vec![0x80, b'x'])
        .body(b"hello".to_vec())
        .build()
        .expect("request must build");
    let serialized = serialize_request(&request, LIMITS).expect("request must serialize");
    assert_eq!(
        serialized.bytes,
        b"POST /path?q=yes HTTP/1.1\r\nX-Binary: \x80x\r\nHost: example.test:8080\r\nContent-Length: 5\r\n\r\nhello"
    );
    assert!(!serialized.response_to_head);
    assert!(serialized.permits_reuse);

    let query_only = Request::get("http://example.test?x=1")
        .build()
        .expect("query request must build");
    let serialized = serialize_request(&query_only, LIMITS).expect("request must serialize");
    assert!(serialized.bytes.starts_with(b"GET /?x=1 HTTP/1.1\r\n"));
}

#[test]
fn request_serialization_rejects_ambiguous_framing_and_generated_limit_overflow() {
    for request in [
        Request::post("http://example.test/")
            .header("Content-Length", "2")
            .body(b"one".to_vec())
            .build()
            .expect("portable request must build"),
        Request::post("http://example.test/")
            .header("Transfer-Encoding", "chunked")
            .body(b"one".to_vec())
            .build()
            .expect("portable request must build"),
    ] {
        assert!(serialize_request(&request, LIMITS).is_err());
    }

    let request = Request::get("http://example.test/")
        .build()
        .expect("request must build");
    let error = serialize_request(
        &request,
        HttpLimits {
            header_count: 0,
            ..LIMITS
        },
    )
    .expect_err("generated Host must count");
    assert_eq!(error.limit_kind(), Some(LimitKind::RequestHeaderCount));
}

#[test]
fn streamed_upload_serialization_generates_exact_framing_within_header_limits() {
    let request = Request::post("http://example.test/upload")
        .build()
        .expect("stream upload base request must build");
    let fixed = serialize_request_with_upload(&request, LIMITS, Some(UploadFraming::Fixed(123)))
        .expect("fixed stream head must serialize");
    assert_eq!(
        fixed.bytes,
        b"POST /upload HTTP/1.1\r\nHost: example.test\r\nContent-Length: 123\r\n\r\n"
    );
    let chunked = serialize_request_with_upload(&request, LIMITS, Some(UploadFraming::Chunked))
        .expect("chunked stream head must serialize");
    assert_eq!(
        chunked.bytes,
        b"POST /upload HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n"
    );
    let error = serialize_request_with_upload(
        &request,
        HttpLimits {
            header_count: 1,
            ..LIMITS
        },
        Some(UploadFraming::Fixed(1)),
    )
    .expect_err("generated Host plus framing must count as two fields");
    assert_eq!(error.limit_kind(), Some(LimitKind::RequestHeaderCount));
}

#[test]
fn fragmented_informational_and_fixed_length_response_completes() {
    let response = decode_fragmented(
        false,
        b"HTTP/1.1 103 Early Hints\r\nLink: </x>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Test: yes\r\n\r\nhello",
        false,
    )
    .expect("fragmented fixed response must complete");
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"hello");
    assert!(response.headers().iter().any(|header| {
        header.name().eq_ignore_ascii_case("x-test") && header.value() == b"yes"
    }));
}

#[test]
fn every_two_part_fragmentation_preserves_valid_and_invalid_results() {
    let valid = [
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".as_slice(),
            false,
            b"hello".as_slice(),
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhe\r\n3;ext=yes\r\nllo\r\n0\r\nX-T: y\r\n\r\n"
                .as_slice(),
            false,
            b"hello".as_slice(),
        ),
        (
            b"HTTP/1.0 200 OK\r\n\r\nhello".as_slice(),
            true,
            b"hello".as_slice(),
        ),
    ];
    for (bytes, eof, expected) in valid {
        for split in 0..=bytes.len() {
            let response = decode_at_split(bytes, split, eof)
                .unwrap_or_else(|error| panic!("split {split} failed: {error}"));
            assert_eq!(response.body(), expected, "split {split}");
        }
    }

    let malformed = [
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nxx".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nx".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nxX".as_slice(),
    ];
    for bytes in malformed {
        for split in 0..=bytes.len() {
            let error = decode_at_split(bytes, split, true)
                .expect_err("malformed response must fail at every split");
            assert_eq!(
                error.transport_stage(),
                Some(TransportStage::Http),
                "split {split}: {error}"
            );
        }
    }
}

#[test]
fn chunk_extensions_and_trailers_are_framed_but_trailers_are_not_exposed() {
    let response = decode_fragmented(
        false,
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;lab=yes\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: yes\r\n\r\n",
        false,
    )
    .expect("chunked response must complete");
    assert_eq!(response.body(), b"hello world");
    assert_eq!(response.headers().len(), 1);
}

#[test]
fn no_body_and_close_delimited_responses_use_explicit_completion_rules() {
    let head = decode_fragmented(
        true,
        b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\n",
        false,
    )
    .expect("HEAD response must complete at the head");
    assert!(head.body().is_empty());

    let no_content = decode_fragmented(false, b"HTTP/1.1 204 No Content\r\n\r\n", false)
        .expect("204 response must complete at the head");
    assert!(no_content.body().is_empty());

    let close = decode_fragmented(
        false,
        b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nbody",
        true,
    )
    .expect("close-delimited response must complete at EOF");
    assert_eq!(close.body(), b"body");
}

#[test]
fn malformed_and_premature_messages_have_portable_stage_mappings() {
    for bytes in [
        &b"NOT HTTP\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 1\r\n\r\n1\r\nx\r\n0\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n"[..],
    ] {
        let error = decode_fragmented(false, bytes, true)
            .expect_err("malformed message must fail");
        assert_eq!(error.transport_stage(), Some(TransportStage::Http));
    }

    let fixed = decode_fragmented(
        false,
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhi",
        true,
    )
    .expect_err("short fixed response must fail");
    assert_eq!(fixed.transport_stage(), Some(TransportStage::Receive));

    let chunked = decode_fragmented(
        false,
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhi",
        true,
    )
    .expect_err("short chunked response must fail");
    assert_eq!(chunked.transport_stage(), Some(TransportStage::Http));
}

#[test]
fn response_limits_fail_before_owned_buffers_cross_their_bounds() {
    let limits = HttpLimits {
        body_bytes: 3,
        header_bytes: 64,
        header_count: 2,
    };
    let mut decoder = ResponseDecoder::new(false, limits);
    let error = decoder
        .ingest(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n")
        .expect_err("declared oversize body must fail at the head");
    assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
    assert!(decoder.body.is_empty());

    let mut decoder = ResponseDecoder::new(false, limits);
    let error = decoder
        .ingest(b"HTTP/1.1 200 OK\r\nX-Long: 1234567890123456789012345678901234567890")
        .expect_err("oversize head must fail while receiving it");
    assert_eq!(error.limit_kind(), Some(LimitKind::ResponseHeaderBytes));
    assert_eq!(decoder.scratch.len(), limits.header_bytes);
}

#[test]
fn response_head_uses_stack_slots_then_honours_larger_configured_count() {
    let mut wire = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n".to_vec();
    for index in 1..40 {
        wire.extend_from_slice(format!("X-{index}: value\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");
    let limits = HttpLimits {
        body_bytes: 1,
        header_bytes: 4096,
        header_count: 40,
    };
    let ParsedHead::Final {
        headers, framing, ..
    } = parse_response_head(&wire, false, limits)
        .expect("configured headers beyond the stack fast path must parse")
    else {
        panic!("response must have a final head")
    };
    assert_eq!(headers.len(), 40);
    assert!(matches!(framing, BodyFraming::Fixed(0)));

    let error = match parse_response_head(
        &wire,
        false,
        HttpLimits {
            header_count: 39,
            ..limits
        },
    ) {
        Err(error) => error,
        Ok(_) => panic!("the configured header-count limit must still fail closed"),
    };
    assert_eq!(error.limit_kind(), Some(LimitKind::ResponseHeaderCount));
}

#[test]
fn decoder_reports_the_exact_boundary_before_trailing_bytes() {
    let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokjunk";
    let expected = wire.len() - b"junk".len();
    let mut decoder = ResponseDecoder::new(false, LIMITS);
    let progress = decoder.ingest(wire).expect("response must decode");
    let response = progress.response.expect("response must complete");
    assert_eq!(response.body(), b"ok");
    assert_eq!(progress.consumed, expected);
    assert_eq!(&wire[progress.consumed..], b"junk");
    assert!(progress.permits_reuse);
}

#[test]
fn decoder_persistence_requires_unambiguous_http_connection_policy() {
    for (wire, expected) in [
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
            true,
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
            false,
        ),
        (
            b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
            false,
        ),
        (
            b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n".as_slice(),
            true,
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: upgrade\r\n\r\n".as_slice(),
            false,
        ),
    ] {
        let mut decoder = ResponseDecoder::new(false, LIMITS);
        let progress = decoder.ingest(wire).expect("policy response must decode");
        assert!(progress.response.is_some());
        assert_eq!(progress.permits_reuse, expected);
    }
}

#[test]
fn native_http_reuses_one_clean_connection_for_sequential_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reuse fixture must bind");
    let address = listener.local_addr().expect("reuse fixture address");
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("reuse fixture must accept once");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("reuse fixture read timeout");
        for body in [b"one".as_slice(), b"two".as_slice()] {
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("reused request must read");
                assert_ne!(read, 0, "client closed the reusable connection early");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .expect("reused response head must write");
            stream
                .write_all(body)
                .expect("reused response body must write");
        }
        release_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("metrics observation must release the reusable peer");
    });
    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("reuse Engine must construct");
    for expected in [b"one".as_slice(), b"two".as_slice()] {
        let response = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/reuse"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("reuse request must build"),
            )
            .expect("reuse request must complete");
        assert_eq!(response.body(), expected);
    }
    let metrics = engine.metrics();
    assert_eq!(metrics.requests_accepted(), 2);
    assert_eq!(metrics.requests_completed(), 2);
    assert_eq!(metrics.connections_opened(), 1);
    assert_eq!(metrics.connections_reused(), 1);
    assert_eq!(metrics.connections_closed(), 0);
    assert_eq!(metrics.current().active_connections(), 1);
    assert_eq!(metrics.current().idle_connections(), 1);
    assert_eq!(metrics.high_water().active_connections(), 1);
    assert_eq!(metrics.high_water().idle_connections(), 1);
    release_tx.send(()).expect("reusable peer must release");
    engine.shutdown().expect("reuse Engine must stop");
    server.join().expect("reuse fixture must join");
}

#[test]
fn configured_zero_idle_limits_or_timeout_disable_reuse() {
    for config in [
        EngineConfig::spawned().with_max_idle_connections(0),
        EngineConfig::spawned().with_max_idle_connections_per_origin(0),
        EngineConfig::spawned().with_idle_connection_timeout(Duration::ZERO),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("no-idle fixture must bind");
        let address = listener.local_addr().expect("no-idle fixture address");
        listener
            .set_nonblocking(true)
            .expect("no-idle listener must become nonblocking");
        let server = thread::spawn(move || {
            for body in [b"one".as_slice(), b"two".as_slice()] {
                let deadline = Instant::now() + Duration::from_secs(2);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(accepted) => break accepted,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "disabled idle retention did not open a replacement socket"
                            );
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("no-idle accept failed: {error}"),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .expect("no-idle accepted socket must become blocking");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("no-idle socket timeout must configure");
                read_request_head(&mut stream, "no-idle request");
                stream
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                            .as_bytes(),
                    )
                    .expect("no-idle response head must write");
                stream
                    .write_all(body)
                    .expect("no-idle response body must write");
                assert_socket_closed(&mut stream, &mut [0_u8; 1], "no-idle retention");
            }
        });
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("no-idle Engine must construct");
        for expected in [b"one".as_slice(), b"two".as_slice()] {
            let response = engine
                .client()
                .execute(
                    Request::get(format!("http://{address}/no-idle"))
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("no-idle request must build"),
                )
                .expect("no-idle request must complete");
            assert_eq!(response.body(), expected);
        }
        engine.shutdown().expect("no-idle Engine must stop");
        server.join().expect("no-idle fixture must join");
    }
}

#[test]
fn manual_native_http_reuses_one_clean_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("manual reuse fixture must bind");
    let address = listener.local_addr().expect("manual reuse address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("manual reuse fixture must accept once");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("manual reuse read timeout");
        for body in [b"one".as_slice(), b"two".as_slice()] {
            read_request_head(&mut stream, "manual reused request");
            stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .expect("manual reused response head must write");
            stream
                .write_all(body)
                .expect("manual reused response body must write");
            stream.flush().expect("manual reused response must flush");
        }
    });
    let config = EngineConfig::manual();
    let backend = NativeHttpBackend::new(
        HttpLimits::from_config(&config),
        None,
        None,
        ConnectionLimits::from_config(&config),
    )
    .expect("manual reuse backend must construct");
    let mut engine = Engine::with_backend(config, Box::new(backend))
        .expect("manual reuse Engine must construct");
    for expected in [b"one".as_slice(), b"two".as_slice()] {
        let pending = engine
            .client()
            .submit(
                Request::get(format!("http://{address}/manual-reuse"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("manual reuse request must build"),
            )
            .expect("manual reuse request must submit");
        let Completion::Completed(response) = engine
            .drive_until(pending)
            .expect("manual reuse request must drive")
        else {
            panic!("manual reused request did not complete");
        };
        assert_eq!(response.body(), expected);
    }
    engine.shutdown().expect("manual reuse Engine must stop");
    server.join().expect("manual reuse fixture must join");
}

#[test]
fn reused_protocol_or_limit_failure_closes_before_replacement() {
    #[derive(Clone, Copy)]
    enum Poison {
        Malformed,
        Oversize,
    }

    for poison in [Poison::Malformed, Poison::Oversize] {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("reused contamination fixture must bind");
        let address = listener.local_addr().expect("reused contamination address");
        let server = thread::spawn(move || {
            let (mut first, _) = listener
                .accept()
                .expect("reused contamination first socket must accept");
            first
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("reused contamination first timeout");
            read_request_head(&mut first, "first clean request");
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("first clean response must write");
            first.flush().expect("first clean response must flush");
            read_request_head(&mut first, "reused poisoned request");
            let bytes = match poison {
                Poison::Malformed => {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx".as_slice()
                }
                Poison::Oversize => b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nxxxx".as_slice(),
            };
            first
                .write_all(bytes)
                .expect("poisoned response must write");
            first.flush().expect("poisoned response must flush");
            drop(first);

            let (mut replacement, _) = listener
                .accept()
                .expect("replacement socket must accept after poison");
            replacement
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("replacement timeout");
            read_request_head(&mut replacement, "replacement request");
            replacement
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew")
                .expect("replacement response must write");
        });
        let config = EngineConfig::spawned().with_max_response_body_bytes(3);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("reused contamination Engine must construct");
        let request = || {
            Request::get(format!("http://{address}/contamination"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("contamination request must build")
        };
        assert_eq!(
            engine
                .client()
                .execute(request())
                .expect("first clean request must complete")
                .body(),
            b"ok"
        );
        let Err(ExecuteError::Failed(error)) = engine.client().execute(request()) else {
            panic!("reused poisoned response did not fail");
        };
        match poison {
            Poison::Malformed => {
                assert_eq!(error.kind(), ErrorKind::Transport);
                assert_eq!(error.transport_stage(), Some(TransportStage::Http));
            }
            Poison::Oversize => {
                assert_eq!(error.kind(), ErrorKind::Limit);
                assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
            }
        }
        assert_eq!(
            engine
                .client()
                .execute(request())
                .expect("replacement request must complete")
                .body(),
            b"new"
        );
        engine
            .shutdown()
            .expect("reused contamination Engine must stop");
        server
            .join()
            .expect("reused contamination fixture must join");
    }
}

#[test]
fn synthetic_idle_expiry_destroys_the_socket_and_releases_capacity() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("idle expiry fixture must bind");
    let address = listener.local_addr().expect("idle expiry address");
    let config = EngineConfig::manual().with_idle_connection_timeout(Duration::from_millis(17));
    let connection_limits = ConnectionLimits::from_config(&config);
    let mut backend = NativeHttpBackend::new(LIMITS, None, None, connection_limits)
        .expect("idle expiry backend must construct");
    let expires_at = Instant::now() + connection_limits.idle_timeout;
    let slot = backend
        .reactor
        .connect(address, Some(expires_at), 1, 1)
        .expect("idle expiry socket must connect");
    let (mut peer, _) = listener.accept().expect("idle expiry peer must accept");
    let key = ConnectionKey {
        scheme: "http".to_owned(),
        host: "127.0.0.1".to_owned(),
        port: address.port(),
        dangerously_disable_tls_verification: false,
    };
    assert!(backend.reserve_connection(&key));
    backend.idle_slots.insert(slot, key.clone());
    backend
        .idle
        .entry(key)
        .or_default()
        .push_back(IdleConnection {
            slot,
            tls: None,
            expires_at,
        });
    backend.idle_count = 1;

    backend.expire_idle(expires_at);

    assert_eq!(backend.idle_count, 0);
    assert_eq!(backend.connection_count, 0);
    assert!(backend.idle.is_empty());
    assert!(backend.idle_slots.is_empty());
    assert!(peer_observed_close(&mut peer));
    backend.shutdown().expect("idle expiry backend must stop");
}

#[test]
fn shutdown_closes_mixed_idle_and_leased_connections() {
    let idle_listener = TcpListener::bind("127.0.0.1:0").expect("mixed idle fixture must bind");
    let idle_address = idle_listener.local_addr().expect("mixed idle address");
    let idle_server = thread::spawn(move || {
        let (mut stream, _) = idle_listener
            .accept()
            .expect("mixed idle socket must accept");
        read_request_head(&mut stream, "mixed idle request");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nidle")
            .expect("mixed idle response must write");
        stream.flush().expect("mixed idle response must flush");
        peer_observed_close(&mut stream)
    });

    let leased_listener = TcpListener::bind("127.0.0.1:0").expect("mixed leased fixture must bind");
    let leased_address = leased_listener.local_addr().expect("mixed leased address");
    let (leased_tx, leased_rx) = mpsc::channel();
    let leased_server = thread::spawn(move || {
        let (mut stream, _) = leased_listener
            .accept()
            .expect("mixed leased socket must accept");
        read_request_head(&mut stream, "mixed leased request");
        leased_tx
            .send(())
            .expect("mixed leased barrier must signal");
        peer_observed_close(&mut stream)
    });

    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("mixed shutdown Engine must construct");
    let idle = engine
        .client()
        .execute(
            Request::get(format!("http://{idle_address}/idle"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("mixed idle request must build"),
        )
        .expect("mixed idle request must complete");
    assert_eq!(idle.body(), b"idle");
    let leased = engine
        .client()
        .submit(
            Request::get(format!("http://{leased_address}/leased"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("mixed leased request must build"),
        )
        .expect("mixed leased request must submit");
    leased_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("mixed leased request must reach the peer");

    engine.shutdown().expect("mixed shutdown Engine must stop");

    assert!(matches!(leased.wait(), Completion::Cancelled));
    assert!(idle_server.join().expect("mixed idle fixture must join"));
    assert!(
        leased_server
            .join()
            .expect("mixed leased fixture must join")
    );
}

#[test]
fn reused_peer_close_fails_without_transparent_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("no-replay fixture must bind");
    let address = listener.local_addr().expect("no-replay fixture address");
    let (no_replay_tx, no_replay_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut first, _) = listener
            .accept()
            .expect("no-replay first socket must accept");
        first
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("no-replay first timeout");
        read_request_head(&mut first, "no-replay first request");
        first
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .expect("no-replay first response must write");
        first.flush().expect("no-replay first response must flush");
        read_request_head(&mut first, "no-replay reused request");
        drop(first);

        listener
            .set_nonblocking(true)
            .expect("no-replay listener must become nonblocking");
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            match listener.accept() {
                Ok(_) => panic!("failed reused request was transparently replayed"),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("no-replay accept probe failed: {error}"),
            }
        }
        listener
            .set_nonblocking(false)
            .expect("no-replay listener must return to blocking");
        no_replay_tx
            .send(())
            .expect("no-replay observation must signal");

        let (mut replacement, _) = listener
            .accept()
            .expect("explicit replacement request must open a socket");
        read_request_head(&mut replacement, "explicit replacement request");
        replacement
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew")
            .expect("explicit replacement response must write");
    });
    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("no-replay Engine must construct");
    let request = || {
        Request::get(format!("http://{address}/no-replay"))
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("no-replay request must build")
    };
    assert_eq!(
        engine
            .client()
            .execute(request())
            .expect("no-replay first request must complete")
            .body(),
        b"ok"
    );
    let Err(ExecuteError::Failed(error)) = engine.client().execute(request()) else {
        panic!("closed reused request did not fail");
    };
    assert_eq!(error.kind(), ErrorKind::Transport);
    assert_eq!(error.transport_stage(), Some(TransportStage::Receive));
    no_replay_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("server must observe no automatic replacement");
    assert_eq!(
        engine
            .client()
            .execute(request())
            .expect("explicit replacement request must complete")
            .body(),
        b"new"
    );
    engine.shutdown().expect("no-replay Engine must stop");
    server.join().expect("no-replay fixture must join");
}

#[test]
fn native_redirects_apply_method_body_relative_and_limit_rules() {
    for status in [301_u16, 302] {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("non-rewritten redirect fixture must bind");
        let address = listener
            .local_addr()
            .expect("non-rewritten redirect address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("non-rewritten redirect must accept");
            let request = read_request_wire(&mut stream, "non-rewritten POST");
            assert!(request.starts_with(b"POST /start HTTP/1.1\r\n"));
            assert!(request.ends_with(b"payload"));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Redirect\r\nLocation: /final\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .expect("non-rewritten redirect must write");
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("non-rewritten redirect Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::post(format!("http://{address}/start"))
                    .body(b"payload".to_vec())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("non-rewritten redirect request must build"),
            )
            .expect("301/302 POST must remain a completed response");
        assert_eq!(response.status(), status);
        engine
            .shutdown()
            .expect("non-rewritten redirect Engine must stop");
        server
            .join()
            .expect("non-rewritten redirect fixture must join");
    }

    for (status, expected_method, expected_body) in [
        (303_u16, "GET", b"".as_slice()),
        (307_u16, "POST", b"payload".as_slice()),
        (308_u16, "POST", b"payload".as_slice()),
    ] {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("followed redirect fixture must bind");
        let address = listener.local_addr().expect("followed redirect address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("followed redirect must accept once");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("followed redirect read timeout");
            let first = read_request_wire(&mut stream, "redirect source request");
            assert!(first.starts_with(b"POST /base/start HTTP/1.1\r\n"));
            assert!(first.ends_with(b"payload"));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Redirect\r\nLocation: ../final?x=1\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .expect("followed redirect must write");
            stream.flush().expect("followed redirect must flush");
            let second = read_request_wire(&mut stream, "redirect target request");
            assert!(
                second.starts_with(format!("{expected_method} /final?x=1 HTTP/1.1\r\n").as_bytes())
            );
            assert_eq!(
                &second[second
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("redirect target head boundary")
                    + 4..],
                expected_body
            );
            let lower = String::from_utf8_lossy(&second).to_ascii_lowercase();
            assert!(lower.contains("authorization: same-origin"));
            if status == 303 {
                assert!(!lower.contains("content-length:"));
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nfinal")
                .expect("redirect final response must write");
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("followed redirect Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::post(format!("http://{address}/base/start"))
                    .header("Authorization", "same-origin")
                    .body(b"payload".to_vec())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("followed redirect request must build"),
            )
            .expect("redirect must complete");
        assert_eq!(response.body(), b"final");
        engine
            .shutdown()
            .expect("followed redirect Engine must stop");
        server.join().expect("followed redirect fixture must join");
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("redirect loop must bind");
    let address = listener.local_addr().expect("redirect loop address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("redirect loop must accept once");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("redirect loop read timeout");
        for _ in 0..3 {
            let request = read_request_wire(&mut stream, "redirect loop request");
            assert!(request.starts_with(b"GET /loop HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 302 Redirect\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n")
                .expect("redirect loop response must write");
            stream.flush().expect("redirect loop response must flush");
        }
    });
    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("redirect loop Engine must construct");
    let result = engine.client().execute(
        Request::get(format!("http://{address}/loop"))
            .redirect_limit(2)
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("redirect loop request must build"),
    );
    let Err(ExecuteError::Failed(error)) = result else {
        panic!("redirect loop did not fail at its hop limit");
    };
    assert_eq!(error.kind(), ErrorKind::Redirect);
    engine.shutdown().expect("redirect loop Engine must stop");
    server.join().expect("redirect loop fixture must join");
}

#[test]
fn native_cross_origin_redirect_strips_origin_bound_credentials() {
    let target_listener = TcpListener::bind("127.0.0.1:0").expect("redirect target must bind");
    let target_address = target_listener
        .local_addr()
        .expect("redirect target address");
    let target_server = thread::spawn(move || {
        let (mut stream, _) = target_listener
            .accept()
            .expect("redirect target must accept");
        let request = read_request_wire(&mut stream, "cross-origin redirect target");
        let lower = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(request.starts_with(b"GET /target HTTP/1.1\r\n"));
        assert!(!lower.contains("authorization:"));
        assert!(!lower.contains("proxy-authorization:"));
        assert!(!lower.contains("cookie:"));
        assert!(!lower.contains("host: caller.invalid"));
        assert!(lower.contains("x-keep: yes"));
        assert!(lower.contains(&format!("host: {target_address}")));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\ntarget")
            .expect("redirect target response must write");
    });

    let source_listener = TcpListener::bind("127.0.0.1:0").expect("redirect source must bind");
    let source_address = source_listener
        .local_addr()
        .expect("redirect source address");
    let source_server = thread::spawn(move || {
        let (mut stream, _) = source_listener
            .accept()
            .expect("redirect source must accept");
        let request = read_request_wire(&mut stream, "cross-origin redirect source");
        let lower = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(lower.contains("authorization: secret"));
        assert!(lower.contains("proxy-authorization: proxy-secret"));
        assert!(lower.contains("cookie: session=secret"));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 302 Redirect\r\nLocation: http://{target_address}/target\r\nContent-Length: 0\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("cross-origin redirect must write");
    });

    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("cross-origin redirect Engine must construct");
    let response = engine
        .client()
        .execute(
            Request::get(format!("http://{source_address}/source"))
                .header("Authorization", "secret")
                .header("Proxy-Authorization", "proxy-secret")
                .header("Cookie", "session=secret")
                .header("Host", "caller.invalid")
                .header("X-Keep", "yes")
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("cross-origin redirect request must build"),
        )
        .expect("cross-origin redirect must complete");
    assert_eq!(response.body(), b"target");
    engine
        .shutdown()
        .expect("cross-origin redirect Engine must stop");
    source_server
        .join()
        .expect("redirect source fixture must join");
    target_server
        .join()
        .expect("redirect target fixture must join");
}

#[test]
fn native_redirect_location_rejects_ambiguity_and_invalid_values() {
    let request = Request::get("http://example.test/base/start")
        .build()
        .expect("redirect source must build");
    let missing = Response::new(302, Vec::new(), Vec::new());
    assert_eq!(
        resolved_redirect_target(&request, &missing)
            .expect("missing Location must be a completed redirect response"),
        None
    );

    let duplicate = Response::new(
        302,
        vec![
            crate::Header::new("Location", "/one"),
            crate::Header::new("location", "/two"),
        ],
        Vec::new(),
    );
    let error = resolved_redirect_target(&request, &duplicate)
        .expect_err("duplicate Location must fail closed");
    assert_eq!(error.kind(), ErrorKind::Redirect);

    let invalid_utf8 = Response::new(
        302,
        vec![crate::Header::new("Location", vec![0xff])],
        Vec::new(),
    );
    let error = resolved_redirect_target(&request, &invalid_utf8)
        .expect_err("non-UTF-8 Location must fail closed");
    assert_eq!(error.kind(), ErrorKind::Redirect);

    let unsupported = Response::new(
        302,
        vec![crate::Header::new("Location", "ftp://example.test/file")],
        Vec::new(),
    );
    let target = resolved_redirect_target(&request, &unsupported)
        .expect("absolute Location must resolve")
        .expect("Location must be present");
    let error = redirected_request(&request, 302, 0, || Ok(Some(target)))
        .expect_err("unsupported redirect scheme must fail closed");
    assert_eq!(error.kind(), ErrorKind::Redirect);
}

#[test]
fn cancel_during_redirected_request_closes_the_active_hop() {
    let target_listener =
        TcpListener::bind("127.0.0.1:0").expect("redirect cancel target must bind");
    let target_address = target_listener
        .local_addr()
        .expect("redirect cancel target address");
    let (active_tx, active_rx) = mpsc::channel();
    let target_server = thread::spawn(move || {
        let (mut stream, _) = target_listener
            .accept()
            .expect("redirect cancel target must accept");
        read_request_head(&mut stream, "redirect cancel target request");
        active_tx
            .send(())
            .expect("redirect cancel barrier must signal");
        assert!(
            peer_observed_close(&mut stream),
            "redirect cancellation must close the active target socket"
        );
    });

    let source_listener =
        TcpListener::bind("127.0.0.1:0").expect("redirect cancel source must bind");
    let source_address = source_listener
        .local_addr()
        .expect("redirect cancel source address");
    let source_server = thread::spawn(move || {
        let (mut stream, _) = source_listener
            .accept()
            .expect("redirect cancel source must accept");
        read_request_head(&mut stream, "redirect cancel source request");
        stream
            .write_all(
                format!(
                    "HTTP/1.1 302 Redirect\r\nLocation: http://{target_address}/stall\r\nContent-Length: 0\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("redirect cancel source must write");
    });

    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("redirect cancel Engine must construct");
    let pending = engine
        .client()
        .submit(
            Request::get(format!("http://{source_address}/start"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("redirect cancel request must build"),
        )
        .expect("redirect cancel request must submit");
    active_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("redirect target must become active");
    pending
        .handle()
        .cancel()
        .expect("redirected request must cancel");
    assert!(matches!(pending.wait(), Completion::Cancelled));
    engine.shutdown().expect("redirect cancel Engine must stop");
    source_server
        .join()
        .expect("redirect cancel source must join");
    target_server
        .join()
        .expect("redirect cancel target must join");
}

#[test]
fn total_timeout_is_not_reset_between_redirect_hops() {
    let target_listener =
        TcpListener::bind("127.0.0.1:0").expect("redirect timeout target must bind");
    let target_address = target_listener
        .local_addr()
        .expect("redirect timeout target address");
    let target_server = thread::spawn(move || {
        let (mut stream, _) = target_listener
            .accept()
            .expect("redirect timeout target must accept");
        read_request_head(&mut stream, "redirect timeout target request");
        assert!(
            peer_observed_close(&mut stream),
            "total timeout must close the redirected target socket"
        );
    });

    let source_listener =
        TcpListener::bind("127.0.0.1:0").expect("redirect timeout source must bind");
    let source_address = source_listener
        .local_addr()
        .expect("redirect timeout source address");
    let source_server = thread::spawn(move || {
        let (mut stream, _) = source_listener
            .accept()
            .expect("redirect timeout source must accept");
        read_request_head(&mut stream, "redirect timeout source request");
        thread::sleep(Duration::from_millis(140));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 302 Redirect\r\nLocation: http://{target_address}/stall\r\nContent-Length: 0\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("redirect timeout source must write");
    });

    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("redirect timeout Engine must construct");
    let started = Instant::now();
    let result = engine.client().execute(
        Request::get(format!("http://{source_address}/start"))
            .total_timeout(Duration::from_millis(220))
            .build()
            .expect("redirect timeout request must build"),
    );
    let elapsed = started.elapsed();
    let Err(ExecuteError::Failed(error)) = result else {
        panic!("redirected request did not retain its original total deadline");
    };
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
    assert!(
        elapsed < Duration::from_millis(320),
        "redirect reset the total timeout: {elapsed:?}"
    );
    engine
        .shutdown()
        .expect("redirect timeout Engine must stop");
    source_server
        .join()
        .expect("redirect timeout source must join");
    target_server
        .join()
        .expect("redirect timeout target must join");
}

#[test]
fn response_connection_close_forces_the_next_request_onto_a_new_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("close fixture must bind");
    listener
        .set_nonblocking(true)
        .expect("close fixture must become nonblocking");
    let address = listener.local_addr().expect("close fixture address");
    let server = thread::spawn(move || {
        for index in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(2);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(accepted) => break accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "next socket was not opened");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("close fixture accept failed: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("close fixture stream must be blocking");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("close request must read");
                assert_ne!(read, 0, "client closed before the close-policy request");
                request.extend_from_slice(&buffer[..read]);
            }
            let connection = if index == 0 {
                "Connection: close\r\n"
            } else {
                ""
            };
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: 1\r\n{connection}\r\n{}",
                        index + 1
                    )
                    .as_bytes(),
                )
                .expect("close-policy response must write");
        }
    });
    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("close-policy Engine must construct");
    for expected in [b"1".as_slice(), b"2".as_slice()] {
        let response = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/close"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("close-policy request must build"),
            )
            .expect("close-policy request must complete");
        assert_eq!(response.body(), expected);
    }
    engine.shutdown().expect("close-policy Engine must stop");
    server.join().expect("close fixture must join");
}

#[test]
fn idle_peer_close_is_evicted_before_the_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("idle-close fixture must bind");
    listener
        .set_nonblocking(true)
        .expect("idle-close fixture must become nonblocking");
    let address = listener.local_addr().expect("idle-close fixture address");
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        for index in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(2);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(accepted) => break accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "replacement socket was not opened"
                        );
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("idle-close fixture accept failed: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("idle-close fixture stream must be blocking");
            stream
                .set_nonblocking(false)
                .expect("idle-close peer must become blocking");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .expect("idle-close request must read");
                assert_ne!(read, 0, "client closed before idle-close request");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n{}", index + 1).as_bytes(),
                )
                .expect("idle-close response must write");
            stream.flush().expect("idle-close response must flush");
            if index == 0 {
                drop(stream);
                closed_tx.send(()).expect("peer close barrier must signal");
            }
        }
    });
    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("idle-close Engine must construct");
    let first = engine
        .client()
        .execute(
            Request::get(format!("http://{address}/idle-close"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("first idle-close request must build"),
        )
        .expect("first idle-close request must complete");
    assert_eq!(first.body(), b"1");
    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("server must close the idle peer");
    thread::sleep(NATIVE_SAFETY_POLL * 3);
    let second = engine
        .client()
        .execute(
            Request::get(format!("http://{address}/idle-close"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("second idle-close request must build"),
        )
        .expect("second idle-close request must use a replacement socket");
    assert_eq!(second.body(), b"2");
    let metrics = engine.metrics();
    assert_eq!(metrics.connections_opened(), 2);
    assert_eq!(metrics.connections_reused(), 0);
    assert!(metrics.connections_closed() >= 1);
    assert!(metrics.idle_connections_evicted() >= 1);
    engine.shutdown().expect("idle-close Engine must stop");
    server.join().expect("idle-close fixture must join");
}

#[test]
fn cancelling_a_reused_request_closes_it_before_the_next_lease() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reuse-cancel fixture must bind");
    let address = listener.local_addr().expect("reuse-cancel fixture address");
    let (second_tx, second_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let read_head = |stream: &mut std::net::TcpStream| {
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .expect("reuse-cancel request must read");
                assert_ne!(read, 0, "client closed before the complete request head");
                request.extend_from_slice(&buffer[..read]);
            }
        };

        let (mut first, _) = listener
            .accept()
            .expect("first reusable socket must accept");
        first
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("first reusable socket timeout");
        read_head(&mut first);
        first
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\none")
            .expect("first reusable response must write");
        first.flush().expect("first reusable response must flush");

        read_head(&mut first);
        second_tx
            .send(())
            .expect("second reused request barrier must signal");
        let mut byte = [0_u8; 1];
        match first.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) => {}
            other => panic!("cancelled reused socket was not closed: {other:?}"),
        }

        let (mut replacement, _) = listener
            .accept()
            .expect("replacement socket after cancellation must accept");
        replacement
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("replacement socket timeout");
        read_head(&mut replacement);
        replacement
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nthree")
            .expect("replacement response must write");
    });
    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("reuse-cancel Engine must construct");
    let first = engine
        .client()
        .execute(
            Request::get(format!("http://{address}/one"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("first reusable request must build"),
        )
        .expect("first reusable request must complete");
    assert_eq!(first.body(), b"one");
    let second = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/two"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("second reusable request must build"),
        )
        .expect("second reusable request must submit");
    second_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("server must observe the reused request");
    second
        .handle()
        .cancel()
        .expect("reused request must cancel");
    assert!(matches!(second.wait(), Completion::Cancelled));
    let third = engine
        .client()
        .execute(
            Request::get(format!("http://{address}/three"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("replacement request must build"),
        )
        .expect("replacement request must complete");
    assert_eq!(third.body(), b"three");
    engine.shutdown().expect("reuse-cancel Engine must stop");
    server.join().expect("reuse-cancel fixture must join");
}

#[test]
fn manual_lease_probe_rejects_unobserved_bytes_before_reuse() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("lease-probe fixture must bind");
    let address = listener.local_addr().expect("lease-probe fixture address");
    let (inject_tx, inject_rx) = std::sync::mpsc::channel();
    let (injected_tx, injected_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let read_head = |stream: &mut std::net::TcpStream| {
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .expect("lease-probe request must read");
                assert_ne!(read, 0, "client closed before lease-probe request head");
                request.extend_from_slice(&buffer[..read]);
            }
        };

        let (mut first, _) = listener
            .accept()
            .expect("first lease-probe socket must accept");
        first
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("first lease-probe timeout");
        read_head(&mut first);
        first
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\none")
            .expect("first lease-probe response must write");
        first
            .flush()
            .expect("first lease-probe response must flush");
        inject_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("test must release the injected bytes");
        first
            .write_all(b"HTTP/1.1 299 Poison\r\nContent-Length: 4\r\n\r\nfake")
            .expect("unobserved bytes must write");
        first.flush().expect("unobserved bytes must flush");
        injected_tx
            .send(())
            .expect("unobserved-byte barrier must signal");

        let mut byte = [0_u8; 1];
        match first.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) => {}
            Ok(_) => return false,
            Err(error) => panic!("lease probe did not close poisoned socket: {error}"),
        }

        let (mut replacement, _) = listener
            .accept()
            .expect("clean replacement socket must accept");
        replacement
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("replacement lease-probe timeout");
        read_head(&mut replacement);
        replacement
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ngood")
            .expect("replacement lease-probe response must write");
        true
    });

    let config = EngineConfig::manual();
    let backend = NativeHttpBackend::new(
        HttpLimits::from_config(&config),
        None,
        None,
        ConnectionLimits::from_config(&config),
    )
    .expect("manual lease-probe backend must construct");
    let mut engine = Engine::with_backend(config, Box::new(backend))
        .expect("manual lease-probe Engine must construct");
    let first = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/one"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("first lease-probe request must build"),
        )
        .expect("first lease-probe request must submit");
    let Completion::Completed(first) = engine
        .drive_until(first)
        .expect("first lease-probe request must drive")
    else {
        panic!("first lease-probe request did not complete");
    };
    assert_eq!(first.body(), b"one");
    inject_tx.send(()).expect("injected bytes must release");
    injected_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("server must inject bytes before the next drive");

    let second = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/two"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("second lease-probe request must build"),
        )
        .expect("second lease-probe request must submit");
    let Completion::Completed(second) = engine
        .drive_until(second)
        .expect("second lease-probe request must drive")
    else {
        panic!("second lease-probe request did not complete");
    };
    assert_eq!(second.status(), 200);
    assert_eq!(second.body(), b"good");
    engine.shutdown().expect("lease-probe Engine must stop");
    assert!(server.join().expect("lease-probe fixture must join"));
}

#[test]
fn connection_queue_starts_the_oldest_eligible_origin_without_starving_its_head() {
    let listener_a = TcpListener::bind("127.0.0.1:0").expect("origin A must bind");
    let address_a = listener_a.local_addr().expect("origin A address");
    let listener_b = TcpListener::bind("127.0.0.1:0").expect("origin B must bind");
    let address_b = listener_b.local_addr().expect("origin B address");
    let (a_ready_tx, a_ready_rx) = std::sync::mpsc::channel();
    let (release_a_tx, release_a_rx) = std::sync::mpsc::channel();
    let server_a = thread::spawn(move || {
        let read_head = |stream: &mut std::net::TcpStream| {
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .expect("origin A request must read");
                assert_ne!(read, 0, "origin A closed before request head");
                request.extend_from_slice(&buffer[..read]);
            }
        };
        let (mut stream, _) = listener_a.accept().expect("origin A must accept once");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("origin A timeout");
        read_head(&mut stream);
        a_ready_tx.send(()).expect("origin A barrier must signal");
        release_a_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("origin A must be released");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na1")
            .expect("origin A first response must write");
        stream.flush().expect("origin A first response must flush");
        read_head(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na2")
            .expect("origin A second response must write");
    });
    let server_b = thread::spawn(move || {
        let (mut stream, _) = listener_b.accept().expect("origin B must accept");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 256];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream
                .read(&mut buffer)
                .expect("origin B request must read");
            assert_ne!(read, 0, "origin B closed before request head");
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nb1")
            .expect("origin B response must write");
    });

    let config = EngineConfig::manual()
        .with_max_connections(std::num::NonZeroUsize::new(2).expect("two is non-zero"))
        .with_max_connections_per_origin(std::num::NonZeroUsize::new(1).expect("one is non-zero"));
    let backend = NativeHttpBackend::new_with_connection_limits(
        HttpLimits::from_config(&config),
        None,
        None,
        ConnectionLimits::from_config(&config),
    )
    .expect("bounded native backend must construct");
    let mut engine = Engine::with_backend(config, Box::new(backend))
        .expect("bounded manual Engine must construct");
    let request = |url: String| {
        Request::get(url)
            .total_timeout(Duration::from_secs(3))
            .build()
            .expect("bounded request must build")
    };
    let a1 = engine
        .client()
        .submit(request(format!("http://{address_a}/a1")))
        .expect("A1 must submit");
    let a2 = engine
        .client()
        .submit(request(format!("http://{address_a}/a2")))
        .expect("A2 must submit");
    let b1 = engine
        .client()
        .submit(request(format!("http://{address_b}/b1")))
        .expect("B1 must submit");
    let Completion::Completed(b1) = engine.drive_until(b1).expect("B1 must drive") else {
        panic!("oldest eligible request on origin B did not complete");
    };
    assert_eq!(b1.body(), b"b1");
    a_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("A1 must already own its connection");
    release_a_tx.send(()).expect("A1 must release");
    let Completion::Completed(a1) = engine.drive_until(a1).expect("A1 must drive") else {
        panic!("A1 did not complete");
    };
    assert_eq!(a1.body(), b"a1");
    let Completion::Completed(a2) = engine.drive_until(a2).expect("A2 must drive") else {
        panic!("queued A2 did not complete");
    };
    assert_eq!(a2.body(), b"a2");
    engine.shutdown().expect("bounded Engine must stop");
    server_a.join().expect("origin A fixture must join");
    server_b.join().expect("origin B fixture must join");
}

#[test]
fn connection_queue_preserves_acceptance_timeouts_without_opening_past_the_cap() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("queue-timeout fixture must bind");
    let address = listener
        .local_addr()
        .expect("queue-timeout fixture address");
    let (first_tx, first_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("queue-timeout fixture must accept once");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("queue-timeout socket timeout");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 256];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream
                .read(&mut buffer)
                .expect("first capped request must read");
            assert_ne!(read, 0, "first capped request closed too soon");
            request.extend_from_slice(&buffer[..read]);
        }
        first_tx.send(()).expect("first capped barrier must signal");
        match stream.read(&mut buffer) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) => {}
            other => panic!("first capped socket was not closed: {other:?}"),
        }
    });
    let config = EngineConfig::manual()
        .with_max_connections(std::num::NonZeroUsize::new(1).expect("one is non-zero"))
        .with_max_connections_per_origin(std::num::NonZeroUsize::new(1).expect("one is non-zero"));
    let backend = NativeHttpBackend::new_with_connection_limits(
        HttpLimits::from_config(&config),
        None,
        None,
        ConnectionLimits::from_config(&config),
    )
    .expect("queue-timeout backend must construct");
    let mut engine = Engine::with_backend(config, Box::new(backend))
        .expect("queue-timeout Engine must construct");
    let first = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/first"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("first capped request must build"),
        )
        .expect("first capped request must submit");
    let second = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/second"))
                .total_timeout(Duration::from_millis(100))
                .build()
                .expect("queued timeout request must build"),
        )
        .expect("queued timeout request must submit");
    let Completion::Failed(error) = engine
        .drive_until(second)
        .expect("queued timeout request must drive")
    else {
        panic!("queued request did not time out");
    };
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
    let metrics = engine.metrics();
    assert_eq!(metrics.requests_accepted(), 2);
    assert_eq!(metrics.requests_failed(), 1);
    assert_eq!(metrics.current().active_connections(), 1);
    assert_eq!(metrics.current().connection_waiters(), 0);
    assert_eq!(metrics.high_water().connection_waiters(), 1);
    first_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first capped request must own the only connection");
    first
        .handle()
        .cancel()
        .expect("first capped request must cancel");
    assert!(matches!(first.wait(), Completion::Cancelled));
    engine.shutdown().expect("queue-timeout Engine must stop");
    server.join().expect("queue-timeout fixture must join");
}

#[test]
fn native_engine_composes_serialization_fragmentation_and_canonical_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
    let address = listener.local_addr().expect("HTTP fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
        let mut received = Vec::new();
        let mut buffer = [0_u8; 256];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("request head must read");
            assert_ne!(read, 0, "client closed before request head");
            received.extend_from_slice(&buffer[..read]);
        }
        let head_end = received
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("request head delimiter")
            + 4;
        while received.len() < head_end + 4 {
            let read = stream.read(&mut buffer).expect("request body must read");
            assert_ne!(read, 0, "client closed before request body");
            received.extend_from_slice(&buffer[..read]);
        }
        assert!(received.starts_with(b"POST /submit?q=1 HTTP/1.1\r\n"));
        assert!(
            received[..head_end]
                .windows(b"Host: ".len())
                .any(|window| window == b"Host: ")
        );
        assert_eq!(&received[head_end..head_end + 4], b"ping");

        let response = b"HTTP/1.1 100 Continue\r\nX-Ignored: yes\r\n\r\nHTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\nX-Final: yes\r\n\r\n4\r\npong\r\n0\r\nX-Trailer: yes\r\n\r\n";
        for byte in response {
            stream
                .write_all(std::slice::from_ref(byte))
                .expect("fragmented response byte must write");
            thread::yield_now();
        }
    });

    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("native HTTP Engine must construct");
    let response = engine
        .client()
        .execute(
            Request::post(format!("http://{address}/submit?q=1#ignored"))
                .body(b"ping".to_vec())
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("native HTTP request must build"),
        )
        .expect("native HTTP request must complete");
    assert_eq!(response.status(), 201);
    assert_eq!(response.body(), b"pong");
    assert!(response.headers().iter().any(|header| {
        header.name().eq_ignore_ascii_case("x-final") && header.value() == b"yes"
    }));
    assert!(
        !response
            .headers()
            .iter()
            .any(|header| header.name().eq_ignore_ascii_case("x-trailer"))
    );
    engine.shutdown().expect("native HTTP Engine must stop");
    server.join().expect("HTTP fixture must join");
}

#[test]
fn public_stream_reader_drains_a_backpressured_native_cleartext_response() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("stream fixture must bind");
    let address = listener.local_addr().expect("stream fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("stream fixture must accept");
        read_request_head(&mut stream, "stream request head");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabcdefghij")
            .expect("stream response must write");
    });

    let config = EngineConfig::spawned()
        .with_max_stream_queue_bytes_per_request(3)
        .with_max_stream_queued_bytes(3);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("native streaming Engine must construct");
    let mut reader = engine
        .client()
        .submit_stream(
            StreamRequest::get(format!("http://{address}/stream"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("stream request must build"),
        )
        .expect("native stream request must submit");
    assert_eq!(
        reader
            .wait_head()
            .expect("stream head must arrive")
            .status(),
        200
    );
    let mut received = Vec::new();
    let mut hole = [0_u8; 2];
    while let Some(read) = reader.read(&mut hole).expect("stream body must read") {
        received.extend_from_slice(&hole[..read]);
    }
    assert_eq!(received, b"abcdefghij");
    engine
        .shutdown()
        .expect("native streaming Engine must stop");
    server.join().expect("stream fixture must join");
}

#[test]
fn fixed_streamed_upload_pumps_incrementally_over_cleartext() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixed upload fixture must bind");
    let address = listener.local_addr().expect("fixed upload address");
    let (respond_tx, respond_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("fixed upload must accept");
        let request = read_request_wire(&mut stream, "fixed streamed upload");
        let head_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("fixed upload head delimiter")
            + 4;
        let head = std::str::from_utf8(&request[..head_end]).expect("fixed head is UTF-8");
        assert!(head.contains("Content-Length: 6\r\n"));
        assert!(!head.to_ascii_lowercase().contains("transfer-encoding"));
        assert_eq!(&request[head_end..], b"abcdef");
        respond_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("finished producer must release the fixed response");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .expect("fixed upload response must write");
    });

    let (body, mut sender) = UploadBody::fixed(6, 3).expect("fixed pair must construct");
    sender
        .try_push(b"abc".to_vec())
        .expect("first fixed chunk fits");
    let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(3);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("fixed upload Engine must construct");
    let reader = engine
        .client()
        .submit_stream(
            StreamRequest::post(format!("http://{address}/fixed"))
                .body_stream(body)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("fixed streamed request must build"),
        )
        .expect("fixed streamed request must submit");
    push_eventually(&mut sender, b"def".to_vec(), "second fixed chunk");
    sender.finish().expect("fixed sender must finish exactly");
    respond_tx
        .send(())
        .expect("fixed response gate must remain available");
    let response = reader.collect().expect("fixed response must collect");
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"ok");
    engine.shutdown().expect("fixed upload Engine must stop");
    server.join().expect("fixed upload fixture must join");
}

#[test]
fn chunked_streamed_upload_generates_framing_and_terminal_chunk() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("chunked upload fixture must bind");
    let address = listener.local_addr().expect("chunked upload address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("chunked upload must accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("chunked fixture timeout must configure");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 128];
        while !request.windows(5).any(|window| window == b"0\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("chunked upload must read");
            assert_ne!(read, 0, "chunked upload closed before terminal chunk");
            request.extend_from_slice(&buffer[..read]);
        }
        let head_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("chunked upload head delimiter")
            + 4;
        let head = std::str::from_utf8(&request[..head_end]).expect("chunked head is UTF-8");
        assert!(head.contains("Transfer-Encoding: chunked\r\n"));
        assert!(!head.to_ascii_lowercase().contains("content-length"));
        assert_eq!(&request[head_end..], b"2\r\nab\r\n3\r\ncde\r\n0\r\n\r\n");
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .expect("chunked upload response must write");
    });

    let (body, mut sender) = UploadBody::chunked(4).expect("chunked pair must construct");
    sender
        .try_push(b"ab".to_vec())
        .expect("first chunked chunk fits");
    let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("chunked upload Engine must construct");
    let mut reader = engine
        .client()
        .submit_stream(
            StreamRequest::post(format!("http://{address}/chunked"))
                .body_stream(body)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("chunked streamed request must build"),
        )
        .expect("chunked streamed request must submit");
    push_eventually(&mut sender, b"cde".to_vec(), "second chunked chunk");
    sender.finish().expect("chunked sender must finish");
    assert_eq!(
        reader.wait_head().expect("chunked response head").status(),
        204
    );
    assert!(reader.is_eof());
    engine.shutdown().expect("chunked upload Engine must stop");
    server.join().expect("chunked upload fixture must join");
}

#[test]
fn streamed_upload_redirect_is_returned_unfollowed() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upload redirect must bind");
    let address = listener.local_addr().expect("upload redirect address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("upload redirect must accept once");
        let request = read_request_wire(&mut stream, "upload redirect request");
        assert!(request.ends_with(b"data"));
        stream
            .write_all(
                b"HTTP/1.1 303 See Other\r\nContent-Length: 0\r\nLocation: /must-not-follow\r\n\r\n",
            )
            .expect("upload redirect response must write");
        listener
            .set_nonblocking(true)
            .expect("upload redirect listener must become nonblocking");
        thread::sleep(Duration::from_millis(100));
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "a live upload redirect must not open another request"
        );
    });

    let (body, mut sender) = UploadBody::fixed(4, 4).expect("upload redirect pair");
    sender
        .try_push(b"data".to_vec())
        .expect("upload redirect data fits");
    sender.finish().expect("upload redirect producer finishes");
    let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("upload redirect Engine must construct");
    let mut reader = engine
        .client()
        .submit_stream(
            StreamRequest::post(format!("http://{address}/upload"))
                .body_stream(body)
                .redirect_limit(5)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("upload redirect request must build"),
        )
        .expect("upload redirect request must submit");
    assert_eq!(
        reader
            .wait_head()
            .expect("redirect head must arrive")
            .status(),
        303
    );
    assert!(reader.is_eof());
    engine.shutdown().expect("upload redirect Engine must stop");
    server.join().expect("upload redirect fixture must join");
}

#[test]
fn completed_streamed_upload_can_return_a_clean_connection_to_the_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("upload reuse fixture must bind");
    let address = listener.local_addr().expect("upload reuse address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("upload reuse must accept once");
        let first = read_request_wire(&mut stream, "streamed upload before reuse");
        assert!(first.ends_with(b"data"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .expect("upload reuse first response must write");
        let second = read_request_wire(&mut stream, "request after streamed upload");
        assert!(second.starts_with(b"GET /after HTTP/1.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nafter")
            .expect("upload reuse second response must write");
    });

    let (body, mut sender) = UploadBody::fixed(4, 4).expect("upload reuse pair");
    sender
        .try_push(b"data".to_vec())
        .expect("upload reuse data fits");
    sender.finish().expect("upload reuse sender finishes");
    let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("upload reuse Engine must construct");
    let first = engine
        .client()
        .submit_stream(
            StreamRequest::post(format!("http://{address}/upload"))
                .body_stream(body)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("upload reuse stream request must build"),
        )
        .expect("upload reuse stream request must submit")
        .collect()
        .expect("upload reuse stream response must collect");
    assert_eq!(first.body(), b"ok");
    let second = engine
        .client()
        .execute(
            Request::get(format!("http://{address}/after"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("upload reuse second request must build"),
        )
        .expect("upload reuse second request must complete");
    assert_eq!(second.body(), b"after");
    engine.shutdown().expect("upload reuse Engine must stop");
    server.join().expect("upload reuse fixture must join");
}

#[test]
fn early_final_response_closes_streamed_upload_and_keeps_http_result() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("early response fixture must bind");
    let address = listener.local_addr().expect("early response address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("early response must accept");
        read_request_head(&mut stream, "early response request head");
        stream
            .write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 6\r\n\r\nreject")
            .expect("early response must write");
        drain_until_socket_closed(&mut stream, "early final response");
    });

    const EARLY_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
    let (body, mut sender) =
        UploadBody::fixed(EARLY_UPLOAD_BYTES as u64, 3).expect("early upload pair");
    let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(3);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("early response Engine must construct");
    let mut reader = engine
        .client()
        .submit_stream(
            StreamRequest::post(format!("http://{address}/reject"))
                .body_stream(body)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("early response request must build"),
        )
        .expect("early response request must submit");
    let producer = thread::spawn(move || sender.push(vec![b'x'; EARLY_UPLOAD_BYTES]));
    assert_eq!(reader.wait_head().expect("413 head must win").status(), 413);
    let error = producer
        .join()
        .expect("early producer thread must join")
        .expect_err("early response must wake and close blocking producer");
    assert_eq!(error.kind(), TryPushErrorKind::Closed);
    assert!(!error.into_chunk().is_empty());
    let response = reader
        .collect()
        .expect("early response body must remain readable under backpressure");
    assert_eq!(response.body(), b"reject");
    engine.shutdown().expect("early response Engine must stop");
    server.join().expect("early response fixture must join");
}

#[test]
fn streaming_consumer_backpressure_pauses_inactivity_but_not_delivery() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("pressure fixture must bind");
    let address = listener.local_addr().expect("pressure fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("pressure fixture must accept");
        read_request_head(&mut stream, "pressure request head");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabcdef")
            .expect("pressure response must write");
    });

    let config = EngineConfig::spawned()
        .with_max_stream_queue_bytes_per_request(3)
        .with_max_stream_queued_bytes(3);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("pressure Engine must construct");
    let mut reader = engine
        .client()
        .submit_stream(
            StreamRequest::get(format!("http://{address}/pressure"))
                .inactivity_timeout(Duration::from_millis(75))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("pressure request must build"),
        )
        .expect("pressure request must submit");
    reader.wait_head().expect("pressure head must arrive");
    let fill_deadline = Instant::now() + Duration::from_secs(1);
    while reader.queued_bytes_for_test() != 3 {
        assert!(
            Instant::now() < fill_deadline,
            "pressure response queue did not reach its exact full state"
        );
        thread::yield_now();
    }
    thread::sleep(Duration::from_millis(225));
    let response = reader
        .collect()
        .expect("consumer backpressure must suppress inactivity timeout");
    assert_eq!(response.body(), b"abcdef");
    engine.shutdown().expect("pressure Engine must stop");
    server.join().expect("pressure fixture must join");
}

#[test]
fn streaming_consumer_backpressure_does_not_pause_total_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("total fixture must bind");
    let address = listener.local_addr().expect("total fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("total fixture must accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("total fixture timeout must configure");
        read_request_head(&mut stream, "total request head");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc")
            .expect("total partial response must write");
        assert_socket_closed(&mut stream, &mut [0_u8; 1], "stream total timeout");
    });
    let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(3);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("total Engine must construct");
    let mut reader = engine
        .client()
        .submit_stream(
            StreamRequest::get(format!("http://{address}/total"))
                .total_timeout(Duration::from_millis(100))
                .build()
                .expect("total request must build"),
        )
        .expect("total request must submit");
    reader.wait_head().expect("total head must arrive");
    thread::sleep(Duration::from_millis(200));
    let Err(crate::StreamError::Failed(error)) = reader.try_read(&mut [0_u8; 1]) else {
        panic!("total timeout must fail a backpressured stream")
    };
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
    engine.shutdown().expect("total Engine must stop");
    server.join().expect("total fixture must join");
}

#[test]
fn aggregate_stream_pressure_releases_windows_after_cancel_and_drain() {
    const INITIAL: usize = 8;
    const REPLACEMENTS: usize = 2;
    const WINDOW: usize = 64;
    const BODY: usize = 4096;

    let listener = TcpListener::bind("127.0.0.1:0").expect("stream pressure must bind");
    let address = listener.local_addr().expect("stream pressure address");
    let server = thread::spawn(move || {
        let mut handlers = Vec::with_capacity(INITIAL + REPLACEMENTS);
        for _ in 0..INITIAL + REPLACEMENTS {
            let (mut stream, _) = listener.accept().expect("stream pressure must accept");
            handlers.push(thread::spawn(move || {
                read_request_head(&mut stream, "stream pressure request head");
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {BODY}\r\nConnection: close\r\n\r\n"
                );
                stream
                    .write_all(head.as_bytes())
                    .expect("stream pressure head must write");
                stream
                    .write_all(&vec![b'x'; BODY])
                    .expect("stream pressure body must write");
            }));
        }
        for handler in handlers {
            handler.join().expect("stream pressure handler must join");
        }
    });

    let config = EngineConfig::spawned()
        .with_max_connections(
            std::num::NonZeroUsize::new(INITIAL).expect("initial count is non-zero"),
        )
        .with_max_connections_per_origin(
            std::num::NonZeroUsize::new(INITIAL).expect("initial count is non-zero"),
        )
        .with_max_idle_connections(0)
        .with_max_idle_connections_per_origin(0)
        .with_max_stream_queue_bytes_per_request(WINDOW)
        .with_max_stream_queued_bytes(INITIAL * WINDOW);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("stream pressure Engine must construct");
    let client = engine.client();
    let mut readers = Vec::with_capacity(INITIAL);
    for index in 0..INITIAL {
        readers.push(
            client
                .submit_stream(
                    StreamRequest::get(format!("http://{address}/initial-{index}"))
                        .total_timeout(Duration::from_secs(5))
                        .build()
                        .expect("stream pressure request must build"),
                )
                .expect("stream pressure request must submit"),
        );
    }
    for reader in &mut readers {
        assert_eq!(
            reader
                .wait_head()
                .expect("every stream pressure head must arrive")
                .status(),
            200
        );
    }
    let fill_deadline = Instant::now() + Duration::from_secs(2);
    while readers
        .iter()
        .any(|reader| reader.queued_bytes_for_test() != WINDOW)
    {
        assert!(
            Instant::now() < fill_deadline,
            "every response window must reach its exact bounded full state"
        );
        thread::yield_now();
    }
    assert_eq!(
        engine.metrics().current().reserved_stream_queue_bytes(),
        INITIAL * WINDOW
    );

    for index in [0, INITIAL / 2] {
        readers[index]
            .handle()
            .cancel()
            .expect("selected full stream must cancel");
    }
    for index in [0, INITIAL / 2] {
        assert!(matches!(
            readers[index].try_read(&mut [0_u8; 1]),
            Err(crate::StreamError::Cancelled)
        ));
    }

    let mut replacements = Vec::with_capacity(REPLACEMENTS);
    for index in 0..REPLACEMENTS {
        replacements.push(
            client
                .submit_stream(
                    StreamRequest::get(format!("http://{address}/replacement-{index}"))
                        .total_timeout(Duration::from_secs(5))
                        .build()
                        .expect("replacement stream must build"),
                )
                .expect("cancelled reservations must admit replacement streams"),
        );
    }

    for (index, reader) in readers.into_iter().enumerate() {
        if index == 0 || index == INITIAL / 2 {
            continue;
        }
        let response = reader
            .collect()
            .expect("surviving full stream must drain to completion");
        assert_eq!(response.body().len(), BODY);
        assert!(response.body().iter().all(|byte| *byte == b'x'));
    }
    for reader in replacements {
        let response = reader
            .collect()
            .expect("replacement stream must drain to completion");
        assert_eq!(response.body().len(), BODY);
    }

    let release_deadline = Instant::now() + Duration::from_secs(1);
    while engine.metrics().current().reserved_stream_queue_bytes() != 0 {
        assert!(
            Instant::now() < release_deadline,
            "terminal stream reservations must be reaped promptly"
        );
        thread::yield_now();
    }
    let metrics = engine.metrics();
    assert_eq!(metrics.requests_accepted(), (INITIAL + REPLACEMENTS) as u64);
    assert_eq!(metrics.requests_completed(), INITIAL as u64);
    assert_eq!(metrics.requests_cancelled(), REPLACEMENTS as u64);
    assert_eq!(metrics.requests_failed(), 0);
    assert_eq!(metrics.current().reserved_stream_queue_bytes(), 0);
    assert_eq!(
        metrics.high_water().reserved_stream_queue_bytes(),
        INITIAL * WINDOW
    );
    assert_eq!(metrics.current().active_connections(), 0);
    engine.shutdown().expect("stream pressure Engine must stop");
    server.join().expect("stream pressure fixture must join");
}

#[test]
fn buffered_stream_request_discards_redirect_body_and_publishes_only_final_response() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("stream redirect must bind");
    let address = listener.local_addr().expect("stream redirect address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("stream redirect must accept");
        read_request_head(&mut stream, "stream redirect first request");
        stream
            .write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 3\r\nLocation: /final\r\n\r\nold")
            .expect("stream redirect response must write");
        read_request_head(&mut stream, "stream redirect final request");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew")
            .expect("stream redirect final response must write");
    });
    let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(2);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("stream redirect Engine must construct");
    let reader = engine
        .client()
        .submit_stream(
            StreamRequest::get(format!("http://{address}/first"))
                .redirect_limit(2)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("stream redirect request must build"),
        )
        .expect("stream redirect request must submit");
    let response = reader.collect().expect("stream redirect must complete");
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"new");
    engine.shutdown().expect("stream redirect Engine must stop");
    server.join().expect("stream redirect fixture must join");
}

#[test]
fn public_stream_no_body_is_immediate_eof_and_decode_error_keeps_its_stage() {
    for malformed in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stream rule fixture must bind");
        let address = listener.local_addr().expect("stream rule fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("stream rule fixture must accept");
            read_request_head(&mut stream, "stream rule request");
            let response = if malformed {
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZ\r\n".as_slice()
            } else {
                b"HTTP/1.1 204 No Content\r\n\r\n".as_slice()
            };
            stream
                .write_all(response)
                .expect("stream rule response must write");
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("stream rule Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("http://{address}/rule"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("stream rule request must build"),
            )
            .expect("stream rule request must submit");
        if malformed {
            assert_eq!(
                reader
                    .wait_head()
                    .expect("malformed response head must publish")
                    .status(),
                200
            );
            let Err(crate::StreamError::Failed(error)) = reader.read(&mut [0_u8; 1]) else {
                panic!("malformed streamed body must fail the reader")
            };
            assert_eq!(error.kind(), ErrorKind::Transport);
            assert_eq!(error.transport_stage(), Some(TransportStage::Http));
            assert_eq!(engine.metrics().requests_failed(), 1);
        } else {
            assert_eq!(
                reader.wait_head().expect("204 head must publish").status(),
                204
            );
            assert!(reader.is_eof());
            assert_eq!(reader.read(&mut [0_u8; 1]).expect("204 must be EOF"), None);
            assert_eq!(engine.metrics().requests_completed(), 1);
        }
        engine.shutdown().expect("stream rule Engine must stop");
        server.join().expect("stream rule fixture must join");
    }
}

#[test]
fn stream_cancel_and_engine_shutdown_close_stalled_sockets_and_readers() {
    for shutdown in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("cancel fixture must bind");
        let address = listener.local_addr().expect("cancel fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("cancel fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("cancel fixture timeout must configure");
            read_request_head(&mut stream, "cancel request head");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc")
                .expect("cancel partial response must write");
            let mut byte = [0_u8; 1];
            assert_socket_closed(&mut stream, &mut byte, "stream cancellation");
        });
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(3);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("cancel Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("http://{address}/cancel"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("cancel request must build"),
            )
            .expect("cancel request must submit");
        reader.wait_head().expect("cancel head must arrive");
        if shutdown {
            engine.shutdown().expect("streaming Engine must shut down");
        } else {
            reader
                .handle()
                .cancel()
                .expect("stream request must cancel");
            engine.shutdown().expect("cancel Engine must stop");
        }
        assert!(matches!(
            reader.try_read(&mut [0_u8; 1]),
            Err(crate::StreamError::Cancelled)
        ));
        server.join().expect("cancel fixture must join");
    }
}

#[test]
fn manual_native_http_engine_uses_the_same_canonical_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
    let address = listener.local_addr().expect("HTTP fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
        let mut received = Vec::new();
        let mut buffer = [0_u8; 256];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("request head must read");
            assert_ne!(read, 0, "client closed before request head");
            received.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nmanual")
            .expect("manual response must write");
    });

    let config = EngineConfig::manual();
    let backend = NativeHttpBackend::new(
        HttpLimits::from_config(&config),
        None,
        None,
        ConnectionLimits::from_config(&config),
    )
    .expect("manual native HTTP backend must construct");
    let mut engine = Engine::with_backend(config, Box::new(backend))
        .expect("manual native HTTP Engine must construct");
    let pending = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/manual"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("manual request must build"),
        )
        .expect("manual request must submit");
    let completion = engine
        .drive_until(pending)
        .expect("manual Engine must drive the HTTP request");
    let Completion::Completed(response) = completion else {
        panic!("manual native HTTP request did not complete");
    };
    assert_eq!(response.body(), b"manual");
    engine
        .shutdown()
        .expect("manual native HTTP Engine must stop");
    server.join().expect("HTTP fixture must join");
}

#[test]
fn manual_native_stream_reader_progresses_only_when_the_owner_drives() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("manual stream fixture must bind");
    let address = listener
        .local_addr()
        .expect("manual stream fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("manual stream fixture must accept");
        read_request_head(&mut stream, "manual stream request head");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nmanual")
            .expect("manual stream response must write");
    });

    let config = EngineConfig::manual().with_max_stream_queue_bytes_per_request(3);
    let backend = NativeHttpBackend::new(
        HttpLimits::from_config(&config),
        None,
        None,
        ConnectionLimits::from_config(&config),
    )
    .expect("manual streaming backend must construct");
    let mut engine = Engine::with_backend(config, Box::new(backend))
        .expect("manual streaming Engine must construct");
    let mut reader = engine
        .client()
        .submit_stream(
            StreamRequest::get(format!("http://{address}/manual-stream"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("manual stream request must build"),
        )
        .expect("manual stream request must submit");
    assert!(
        reader
            .try_head()
            .expect("passive head probe must work")
            .is_none()
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut received = Vec::new();
    let mut buffer = [0_u8; 2];
    loop {
        engine
            .drive((Instant::now() + Duration::from_millis(10)).min(deadline))
            .expect("manual stream drive must succeed");
        match reader
            .try_read(&mut buffer)
            .expect("manual passive read must succeed")
        {
            crate::StreamRead::Pending => {
                assert!(Instant::now() < deadline, "manual stream timed out")
            }
            crate::StreamRead::Data(read) => received.extend_from_slice(&buffer[..read]),
            crate::StreamRead::Eof => break,
        }
    }
    assert_eq!(received, b"manual");
    engine
        .shutdown()
        .expect("manual streaming Engine must stop");
    server.join().expect("manual stream fixture must join");
}

#[test]
fn manual_streamed_upload_uses_try_push_and_owner_drive_only() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("manual upload fixture must bind");
    let address = listener.local_addr().expect("manual upload address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("manual upload must accept");
        let request = read_request_wire(&mut stream, "manual streamed upload");
        let head_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("manual upload head delimiter")
            + 4;
        assert_eq!(&request[head_end..], b"data");
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .expect("manual upload response must write");
    });

    let (body, mut sender) = UploadBody::fixed(4, 4).expect("manual upload pair");
    sender
        .try_push(b"dat".to_vec())
        .expect("manual prequeue fits");
    let config = EngineConfig::manual().with_max_stream_queue_bytes_per_request(4);
    let backend = NativeHttpBackend::new(
        HttpLimits::from_config(&config),
        None,
        None,
        ConnectionLimits::from_config(&config),
    )
    .expect("manual upload backend must construct");
    let mut engine = Engine::with_backend(config, Box::new(backend))
        .expect("manual upload Engine must construct");
    let mut reader = engine
        .client()
        .submit_stream(
            StreamRequest::post(format!("http://{address}/manual-upload"))
                .body_stream(body)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("manual upload request must build"),
        )
        .expect("manual upload request must submit");
    let error = sender
        .push(b"a".to_vec())
        .expect_err("manual blocking push must fail");
    assert_eq!(error.kind(), TryPushErrorKind::WrongMode);
    assert_eq!(error.into_chunk(), b"a");
    sender
        .try_push(b"a".to_vec())
        .expect("manual try_push works");
    sender.finish().expect("manual upload must finish");

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        engine
            .drive((Instant::now() + Duration::from_millis(10)).min(deadline))
            .expect("manual upload drive must succeed");
        match reader.try_head() {
            Ok(Some(head)) => {
                assert_eq!(head.status(), 204);
                assert!(reader.is_eof());
                break;
            }
            Ok(None) => assert!(Instant::now() < deadline, "manual upload timed out"),
            Err(error) => panic!("manual upload failed: {error}"),
        }
    }
    engine.shutdown().expect("manual upload Engine must stop");
    server.join().expect("manual upload fixture must join");
}

#[test]
fn abandoned_and_length_mismatched_uploads_fail_at_send_stage() {
    for abandon in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("producer failure must bind");
        let address = listener.local_addr().expect("producer failure address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("producer failure must accept");
            if !peer_observed_close(&mut stream) {
                assert_socket_closed(&mut stream, &mut [0_u8; 1024], "producer failure");
            }
        });
        let (body, sender) = UploadBody::fixed(4, 4).expect("producer failure pair");
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("producer failure Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("http://{address}/producer-failure"))
                    .body_stream(body)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("producer failure request must build"),
            )
            .expect("producer failure request must submit");
        if abandon {
            drop(sender);
        } else {
            let error = sender
                .finish()
                .expect_err("short fixed producer must reject finish");
            assert_eq!(error.kind(), crate::UploadFinishErrorKind::LengthMismatch);
        }
        let Err(crate::StreamError::Failed(error)) = reader.wait_head() else {
            panic!("producer failure must fail the reader")
        };
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert_eq!(error.transport_stage(), Some(TransportStage::Send));
        engine
            .shutdown()
            .expect("producer failure Engine must stop");
        server.join().expect("producer failure fixture must join");
    }
}

#[test]
fn cancel_and_shutdown_wake_blocked_upload_producers() {
    for shutdown in [false, true] {
        const BODY_BYTES: usize = 8 * 1024 * 1024;
        let listener = TcpListener::bind("127.0.0.1:0").expect("upload stop fixture must bind");
        let address = listener.local_addr().expect("upload stop address");
        let (head_tx, head_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("upload stop must accept");
            read_request_head(&mut stream, "upload stop request head");
            head_tx.send(()).expect("upload stop head must signal");
            // Cancellation closes the socket promptly, but bytes already accepted by the
            // kernel before the terminal winner may still reach the peer first. Drain those
            // bounded in-flight bytes and require the close rather than pretending cancel can
            // recall a completed socket write.
            drain_until_socket_closed(&mut stream, "upload stop");
        });
        let (body, mut sender) =
            UploadBody::fixed(BODY_BYTES as u64, 1024).expect("upload stop pair");
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(1024);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("upload stop Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("http://{address}/upload-stop"))
                    .body_stream(body)
                    .total_timeout(Duration::from_secs(3))
                    .build()
                    .expect("upload stop request must build"),
            )
            .expect("upload stop request must submit");
        let producer = thread::spawn(move || sender.push(vec![b'x'; BODY_BYTES]));
        head_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("upload stop server must observe head");
        if shutdown {
            engine
                .shutdown()
                .expect("upload Engine shutdown must complete");
        } else {
            reader
                .handle()
                .cancel()
                .expect("upload cancellation must win");
            engine
                .shutdown()
                .expect("cancelled upload Engine must stop");
        }
        let error = producer
            .join()
            .expect("blocked upload producer must join")
            .expect_err("blocked upload producer must wake closed");
        assert_eq!(error.kind(), TryPushErrorKind::Closed);
        assert!(!error.into_chunk().is_empty());
        assert!(matches!(
            reader.try_head(),
            Err(crate::StreamError::Cancelled)
        ));
        server.join().expect("upload stop fixture must join");
    }
}

#[test]
fn native_engine_completes_close_delimited_body_on_peer_fin() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
    let address = listener.local_addr().expect("HTTP fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
        let mut received = Vec::new();
        let mut buffer = [0_u8; 256];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("request head must read");
            assert_ne!(read, 0, "client closed before request head");
            received.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nclose-body")
            .expect("close-delimited response must write");
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("response write half must close");
        assert_socket_closed(&mut stream, &mut buffer, "close-delimited completion");
    });

    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("native HTTP Engine must construct");
    let response = engine
        .client()
        .execute(
            Request::get(format!("http://{address}/close"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("close-delimited request must build"),
        )
        .expect("close-delimited response must complete");
    assert_eq!(response.body(), b"close-body");
    engine.shutdown().expect("native HTTP Engine must stop");
    server.join().expect("HTTP fixture must join");
}

#[test]
fn native_engine_cancellation_closes_a_stalled_http_socket_promptly() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
    let address = listener.local_addr().expect("HTTP fixture address");
    let (head_tx, head_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("fixture timeout must configure");
        let mut received = Vec::new();
        let mut buffer = [0_u8; 256];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("request head must read");
            assert_ne!(read, 0, "client closed before request head");
            received.extend_from_slice(&buffer[..read]);
        }
        head_tx.send(()).expect("head observation must send");
        assert_socket_closed(&mut stream, &mut buffer, "stalled request cancellation");
    });

    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("native HTTP Engine must construct");
    let pending = engine
        .client()
        .submit(
            Request::get(format!("http://{address}/stall"))
                .total_timeout(Duration::from_secs(5))
                .build()
                .expect("native HTTP request must build"),
        )
        .expect("native HTTP request must submit");
    head_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("server must observe request head");
    let started = Instant::now();
    pending
        .handle()
        .cancel()
        .expect("request cancellation must win");
    assert!(matches!(pending.wait(), Completion::Cancelled));
    assert!(started.elapsed() < Duration::from_millis(500));
    engine.shutdown().expect("native HTTP Engine must stop");
    server.join().expect("HTTP fixture must join");
}

#[test]
fn capped_connection_pressure_survives_mixed_peer_interruptions() {
    const BATCH: usize = 64;
    const STALLED: usize = BATCH / 8;
    const INTERRUPTED: usize = BATCH / 8;

    let listener = TcpListener::bind("127.0.0.1:0").expect("pressure fixture must bind");
    let address = listener.local_addr().expect("pressure fixture address");
    let (stalled_tx, stalled_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut handlers = Vec::with_capacity(BATCH + 1);
        for _ in 0..=BATCH {
            let (mut stream, _) = listener.accept().expect("pressure fixture must accept");
            let stalled_tx = stalled_tx.clone();
            handlers.push(thread::spawn(move || {
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .expect("pressure socket timeout must configure");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 512];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).expect("pressure request must read");
                    assert_ne!(read, 0, "pressure client closed before its request head");
                    request.extend_from_slice(&buffer[..read]);
                }
                let first_line_end = request
                    .windows(2)
                    .position(|window| window == b"\r\n")
                    .expect("pressure request must have a first line");
                let first_line = std::str::from_utf8(&request[..first_line_end])
                    .expect("pressure request line must be UTF-8");
                let path = first_line
                    .split_ascii_whitespace()
                    .nth(1)
                    .expect("pressure request must have a target");
                if path == "/health" {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .expect("health response must write");
                    return;
                }
                let index = path
                    .strip_prefix('/')
                    .expect("pressure target must be origin-form")
                    .parse::<usize>()
                    .expect("pressure target must contain its index");
                match index % 8 {
                    0 => {
                        stalled_tx
                            .send(index)
                            .expect("stalled pressure request must signal");
                        assert_socket_closed(
                            &mut stream,
                            &mut buffer,
                            "cancelled pressure request",
                        );
                    }
                    1 => {
                        stream
                            .shutdown(std::net::Shutdown::Both)
                            .expect("interrupted pressure socket must close");
                    }
                    _ => {
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                            )
                            .expect("pressure response must write");
                    }
                }
            }));
        }
        for handler in handlers {
            handler
                .join()
                .expect("pressure connection handler must join");
        }
    });

    let config = EngineConfig::spawned()
        .with_max_connections(std::num::NonZeroUsize::new(4).expect("four is non-zero"))
        .with_max_connections_per_origin(std::num::NonZeroUsize::new(4).expect("four is non-zero"))
        .with_max_idle_connections(0)
        .with_max_idle_connections_per_origin(0);
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("pressure Engine must construct");
    let client = engine.client();
    let mut pending = Vec::with_capacity(BATCH);
    let mut handles = Vec::with_capacity(BATCH);
    for index in 0..BATCH {
        let request = client
            .submit(
                Request::get(format!("http://{address}/{index}"))
                    .total_timeout(Duration::from_secs(5))
                    .build()
                    .expect("pressure request must build"),
            )
            .expect("pressure request must submit");
        handles.push(request.handle());
        pending.push(request);
    }
    for _ in 0..STALLED {
        let index = stalled_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("every stalled request must reach the peer");
        handles[index]
            .cancel()
            .expect("stalled pressure request must cancel");
    }

    let mut completed = 0;
    let mut failed = 0;
    let mut cancelled = 0;
    for request in pending {
        match request.wait() {
            Completion::Completed(response) => {
                assert_eq!(response.status(), 200);
                assert_eq!(response.body(), b"ok");
                completed += 1;
            }
            Completion::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::Transport);
                assert_eq!(error.transport_stage(), Some(TransportStage::Receive));
                failed += 1;
            }
            Completion::Cancelled => cancelled += 1,
        }
    }
    assert_eq!(completed, BATCH - STALLED - INTERRUPTED);
    assert_eq!(failed, INTERRUPTED);
    assert_eq!(cancelled, STALLED);

    let health = client
        .execute(
            Request::get(format!("http://{address}/health"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("health request must build"),
        )
        .expect("owner must remain healthy after mixed interruptions");
    assert_eq!(health.body(), b"ok");

    let metrics = engine.metrics();
    assert_eq!(metrics.requests_accepted(), (BATCH + 1) as u64);
    assert_eq!(metrics.requests_completed(), (completed + 1) as u64);
    assert_eq!(metrics.requests_failed(), failed as u64);
    assert_eq!(metrics.requests_cancelled(), cancelled as u64);
    assert_eq!(metrics.current().active_connections(), 0);
    assert_eq!(metrics.current().connection_waiters(), 0);
    assert_eq!(metrics.high_water().active_connections(), 4);
    assert!(metrics.high_water().connection_waiters() > 0);
    assert_eq!(metrics.connections_opened(), (BATCH + 1) as u64);
    assert_eq!(metrics.connections_closed(), (BATCH + 1) as u64);
    engine.shutdown().expect("pressure Engine must stop");
    server.join().expect("pressure fixture must join");
}

#[test]
fn native_http_stalled_response_classifies_inactivity_and_total_timeouts() {
    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("native HTTP Engine must construct");
    let client = engine.client();

    for timeout_kind in [TimeoutKind::Inactivity, TimeoutKind::Total] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let address = listener.local_addr().expect("HTTP fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("fixture timeout must configure");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 256];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("request head must read");
                assert_ne!(read, 0, "client closed before request head");
                received.extend_from_slice(&buffer[..read]);
            }
            assert_socket_closed(&mut stream, &mut buffer, "HTTP timeout");
        });

        let builder = Request::get(format!("http://{address}/timeout"));
        let request = match timeout_kind {
            TimeoutKind::Inactivity => builder
                .inactivity_timeout(Duration::from_millis(40))
                .total_timeout(Duration::from_secs(2)),
            TimeoutKind::Total => builder.total_timeout(Duration::from_millis(40)),
            _ => panic!("unexpected timeout fixture kind"),
        }
        .build()
        .expect("timeout request must build");
        let started = Instant::now();
        let error = client
            .execute(request)
            .expect_err("stalled native HTTP response must time out");
        let ExecuteError::Failed(error) = error else {
            panic!("timeout request returned the wrong terminal category");
        };
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(error.timeout_kind(), Some(timeout_kind));
        server.join().expect("timeout fixture must join");
        assert!(started.elapsed() < Duration::from_millis(500));
    }
    engine.shutdown().expect("native HTTP Engine must stop");
}

#[test]
fn useful_response_progress_refreshes_the_inactivity_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
    let address = listener.local_addr().expect("HTTP fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
        let mut received = Vec::new();
        let mut buffer = [0_u8; 256];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("request head must read");
            assert_ne!(read, 0, "client closed before request head");
            received.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n")
            .expect("response head must write");
        for byte in b"progress" {
            thread::sleep(Duration::from_millis(50));
            stream
                .write_all(std::slice::from_ref(byte))
                .expect("progress byte must write");
            stream.flush().expect("progress byte must flush");
        }
    });

    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("native HTTP Engine must construct");
    let response = engine
        .client()
        .execute(
            Request::get(format!("http://{address}/progress"))
                .inactivity_timeout(Duration::from_millis(200))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("progress request must build"),
        )
        .expect("useful progress must keep the request alive");
    assert_eq!(response.body(), b"progress");
    engine.shutdown().expect("native HTTP Engine must stop");
    server.join().expect("HTTP fixture must join");
}

#[test]
fn cancellation_closes_every_http_parse_boundary() {
    let boundaries = [
        ("before response", b"".as_slice()),
        ("status line", b"HTTP/1.".as_slice()),
        ("header value", b"HTTP/1.1 200 OK\r\nX-Test: par".as_slice()),
        (
            "before fixed body",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n".as_slice(),
        ),
        (
            "inside fixed body",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhe".as_slice(),
        ),
        (
            "chunk size",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nA".as_slice(),
        ),
        (
            "chunk data",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhe".as_slice(),
        ),
        (
            "chunk terminator",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r".as_slice(),
        ),
        (
            "trailers",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n0\r\nX-Trailer: par"
                .as_slice(),
        ),
    ];
    let config = EngineConfig::spawned();
    let engine =
        Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
            .expect("native HTTP Engine must construct");
    let client = engine.client();

    for (index, (name, prefix)) in boundaries.into_iter().enumerate() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let address = listener.local_addr().expect("HTTP fixture address");
        let (stalled_tx, stalled_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("fixture timeout must configure");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 256];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("request head must read");
                assert_ne!(read, 0, "client closed before request head");
                received.extend_from_slice(&buffer[..read]);
            }
            stream.write_all(prefix).expect("stalled prefix must write");
            stream.flush().expect("stalled prefix must flush");
            stalled_tx.send(()).expect("stall observation must send");
            assert_socket_closed(&mut stream, &mut buffer, name);
        });

        let pending = client
            .submit(
                Request::get(format!("http://{address}/case-{index}"))
                    .total_timeout(Duration::from_secs(5))
                    .build()
                    .expect("boundary request must build"),
            )
            .expect("boundary request must submit");
        stalled_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server must reach parse boundary");
        let started = Instant::now();
        pending.handle().cancel().expect("boundary cancel must win");
        assert!(matches!(pending.wait(), Completion::Cancelled), "{name}");
        server.join().expect("boundary fixture must join");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{name}: network cancellation took {:?}",
            started.elapsed()
        );
    }
    engine.shutdown().expect("native HTTP Engine must stop");
}
