#![cfg(feature = "curl-pilot")]

use std::io::{ErrorKind as IoErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nbreq::{Engine, ErrorKind, ExecuteError, Request, TransportStage};
use socket2::SockRef;

#[derive(Clone, Copy)]
enum Script {
    Bytes(&'static [u8]),
    Fragmented(&'static [u8]),
    ResetDuringUpload,
    TwoKeepAliveResponses,
}

struct ScriptedServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ScriptedServer {
    fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("lab listener must bind");
        listener
            .set_nonblocking(true)
            .expect("lab listener must become nonblocking");
        let address = listener.local_addr().expect("lab listener address");
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker_stopping.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _peer)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("accepted lab stream must become blocking");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .expect("lab read timeout must configure");
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .expect("lab write timeout must configure");
                        stream
                            .set_nodelay(true)
                            .expect("TCP_NODELAY must configure");
                        match script {
                            Script::Bytes(response) => {
                                read_request_head(&mut stream);
                                stream.write_all(response).expect("lab response must write");
                            }
                            Script::Fragmented(response) => {
                                read_request_head(&mut stream);
                                for byte in response {
                                    stream
                                        .write_all(std::slice::from_ref(byte))
                                        .expect("fragmented response byte must write");
                                    stream.flush().expect("fragmented response must flush");
                                    thread::yield_now();
                                }
                            }
                            Script::ResetDuringUpload => {
                                let socket = SockRef::from(&stream);
                                socket
                                    .set_recv_buffer_size(1024)
                                    .expect("small receive buffer must configure");
                                socket
                                    .set_linger(Some(Duration::ZERO))
                                    .expect("abortive close must configure");
                            }
                            Script::TwoKeepAliveResponses => {
                                for (body, connection) in [
                                    (b"one".as_slice(), "keep-alive"),
                                    (b"two".as_slice(), "close"),
                                ] {
                                    read_request_head(&mut stream);
                                    let head = format!(
                                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n",
                                        body.len()
                                    );
                                    stream
                                        .write_all(head.as_bytes())
                                        .expect("keep-alive response head must write");
                                    stream
                                        .write_all(body)
                                        .expect("keep-alive response body must write");
                                    stream.flush().expect("keep-alive response must flush");
                                }
                            }
                        }
                        return;
                    }
                    Err(error) if error.kind() == IoErrorKind::WouldBlock => {
                        thread::yield_now();
                    }
                    Err(error) => panic!("lab listener failed: {error}"),
                }
            }
            if !worker_stopping.load(Ordering::Acquire) {
                panic!("lab server did not receive its request");
            }
        });
        Self {
            address,
            stopping,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/", self.address)
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let joined = worker.join();
            if !thread::panicking() {
                joined.expect("lab server must join");
            }
        }
    }
}

fn read_request_head(stream: &mut TcpStream) {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 512];
    while !received.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream
            .read(&mut buffer)
            .expect("lab request head must read");
        assert_ne!(read, 0, "client closed before sending a request head");
        received.extend_from_slice(&buffer[..read]);
    }
}

fn execute(script: Script) -> Result<nbreq::Response, ExecuteError> {
    let server = ScriptedServer::start(script);
    let engine = Engine::builder()
        .build()
        .expect("lab Engine must construct");
    let result = engine.client().execute(
        Request::get(server.url())
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("lab request must build"),
    );
    engine.shutdown().expect("lab Engine must stop");
    result
}

fn assert_transport(case: &str, script: Script, expected: TransportStage) {
    match execute(script).expect_err("adversarial response must fail") {
        ExecuteError::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Transport, "{case}: {error}");
            assert_eq!(error.transport_stage(), Some(expected), "{case}: {error}");
        }
        other => panic!("{case}: expected terminal transport failure, got {other:?}"),
    }
}

#[test]
fn fragmented_and_chunked_responses_complete_through_the_public_api() {
    let fragmented = execute(Script::Fragmented(
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
    ))
    .expect("fragmented response must complete");
    assert_eq!(fragmented.status(), 200);
    assert_eq!(fragmented.body(), b"hello");

    let chunked = execute(Script::Bytes(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5;lab=yes\r\nhello\r\n0\r\nX-Lab-Trailer: yes\r\n\r\n",
    ))
    .expect("chunk extension and trailer response must complete");
    assert_eq!(chunked.status(), 200);
    assert_eq!(chunked.body(), b"hello");
}

#[test]
fn sequential_requests_reuse_one_http11_connection() {
    let server = ScriptedServer::start(Script::TwoKeepAliveResponses);
    let engine = Engine::builder()
        .build()
        .expect("lab Engine must construct");
    let client = engine.client();
    for expected in [b"one".as_slice(), b"two".as_slice()] {
        let response = client
            .execute(
                Request::get(server.url())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("reuse request must build"),
            )
            .expect("reused request must complete");
        assert_eq!(response.body(), expected);
    }
    engine.shutdown().expect("lab Engine must stop");
}

#[test]
fn malformed_status_headers_lengths_and_chunks_map_to_http() {
    for (case, response) in [
        ("invalid status line", &b"NOT-HTTP\r\n\r\n"[..]),
        (
            "invalid header name",
            &b"HTTP/1.1 200 OK\r\nBad Header: value\r\nContent-Length: 0\r\n\r\n"[..],
        ),
        (
            "invalid header value",
            &b"HTTP/1.1 200 OK\r\nX-Lab: value\x01bad\r\nContent-Length: 0\r\n\r\n"[..],
        ),
        (
            "conflicting content lengths",
            &b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx"[..],
        ),
        (
            "invalid chunk size",
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n"[..],
        ),
    ] {
        assert_transport(case, Script::Bytes(response), TransportStage::Http);
    }
}

#[test]
fn premature_eof_and_empty_response_map_to_receive() {
    assert_transport(
        "short fixed-length body",
        Script::Bytes(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort"),
        TransportStage::Receive,
    );
    assert_transport(
        "empty response",
        Script::Bytes(b""),
        TransportStage::Receive,
    );
    assert_transport(
        "short chunk body",
        Script::Bytes(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhi"),
        TransportStage::Http,
    );
}

#[test]
fn abortive_close_during_a_large_upload_maps_to_send() {
    const UPLOAD_BYTES: usize = 64 * 1024 * 1024;
    let engine = Engine::builder()
        .max_request_body_bytes(UPLOAD_BYTES)
        .build()
        .expect("lab Engine must construct");
    let client = engine.client();
    for trial in 0..10 {
        let server = ScriptedServer::start(Script::ResetDuringUpload);
        let result = client.execute(
            Request::post(server.url())
                .body(vec![b'x'; UPLOAD_BYTES])
                .total_timeout(Duration::from_secs(5))
                .build()
                .expect("large upload must build"),
        );
        match result.expect_err("abortive upload close must fail") {
            ExecuteError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::Transport, "trial {trial}: {error}");
                assert_eq!(
                    error.transport_stage(),
                    Some(TransportStage::Send),
                    "trial {trial}: {error}"
                );
            }
            other => panic!("trial {trial}: expected terminal send failure, got {other:?}"),
        }
    }
    engine.shutdown().expect("lab Engine must stop");
}
