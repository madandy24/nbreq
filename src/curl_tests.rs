use std::io::{ErrorKind as IoErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::testing;
use crate::{Completion, EngineConfig, ErrorKind, Request, RequestOptions, TlsVerification};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerEvent {
    SlowHeaders,
    SlowHeadersClosed,
    StalledBody,
    StalledBodyClosed,
}

struct LocalServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
    events: mpsc::Receiver<ServerEvent>,
}

impl LocalServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener must bind");
        listener
            .set_nonblocking(true)
            .expect("test listener must become nonblocking");
        let address = listener
            .local_addr()
            .expect("listener must have an address");
        let stopping = Arc::new(AtomicBool::new(false));
        let listener_stopping = Arc::clone(&stopping);
        let connections = Arc::new(Mutex::new(Vec::new()));
        let listener_connections = Arc::clone(&connections);
        let (events_tx, events) = mpsc::channel();
        let listener_thread = thread::spawn(move || {
            while !listener_stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        if listener_stopping.load(Ordering::Acquire) {
                            break;
                        }
                        stream
                            .set_nonblocking(false)
                            .expect("accepted test stream must become blocking");
                        let connection_stopping = Arc::clone(&listener_stopping);
                        let connection_events = events_tx.clone();
                        let connection = thread::spawn(move || {
                            serve_connection(stream, &connection_stopping, &connection_events);
                        });
                        listener_connections
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(connection);
                    }
                    Err(error) if error.kind() == IoErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("test listener failed: {error}"),
                }
            }
        });
        Self {
            address,
            stopping,
            listener: Some(listener_thread),
            connections,
            events,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn expect_event(&self, expected: ServerEvent) {
        assert_eq!(
            self.events
                .recv_timeout(Duration::from_secs(2))
                .expect("server event must arrive"),
            expected
        );
    }

    fn expect_event_within(&self, expected: ServerEvent, duration: Duration) -> Duration {
        let started = Instant::now();
        assert_eq!(
            self.events
                .recv_timeout(duration)
                .expect("server event must arrive within the latency bound"),
            expected
        );
        started.elapsed()
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _wake_listener = TcpStream::connect(self.address);
        if let Some(listener) = self.listener.take() {
            listener.join().expect("test listener must join");
        }
        for connection in self
            .connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
        {
            connection.join().expect("test connection must join");
        }
    }
}

fn serve_connection(
    mut stream: TcpStream,
    stopping: &AtomicBool,
    events: &mpsc::Sender<ServerEvent>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("test stream timeout must configure");
    let request = read_request(&mut stream);
    let request_line = request
        .split(|byte| *byte == b'\n')
        .next()
        .expect("request line must exist");
    let request_line = String::from_utf8_lossy(request_line);
    let mut parts = request_line.split_whitespace();
    let method = parts.next().expect("request method must exist");
    let path = parts.next().expect("request path must exist");

    match path {
        "/ok" => write_response(&mut stream, 200, b"hello", &[("X-NBReq", "yes")]),
        "/not-found" => write_response(&mut stream, 404, b"missing", &[]),
        "/echo" => {
            assert_eq!(method, "POST");
            let body = request_body(&request);
            write_response(&mut stream, 200, body, &[]);
        }
        "/redirect-302-post" => write_redirect(&mut stream, 302, "/echo"),
        "/redirect-303-post" => write_redirect(&mut stream, 303, "/inspect"),
        "/redirect-307-post" => write_redirect(&mut stream, 307, "/echo"),
        "/redirect-same" => write_redirect(&mut stream, 302, "/inspect-auth"),
        "/redirect-loop" => write_redirect(&mut stream, 302, "/redirect-loop"),
        "/inspect" => {
            let body = format!("method={method};body={}", request_body(&request).len());
            write_response(&mut stream, 200, body.as_bytes(), &[]);
        }
        "/inspect-auth" => {
            let authorization = request_has_header(&request, "authorization");
            let cookie = request_has_header(&request, "cookie");
            let body = format!("authorization={authorization};cookie={cookie}");
            write_response(&mut stream, 200, body.as_bytes(), &[]);
        }
        "/stall-headers" => watch_stalled_connection(
            stream,
            stopping,
            events,
            ServerEvent::SlowHeaders,
            ServerEvent::SlowHeadersClosed,
        ),
        "/stall-body" => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\nx",
                )
                .expect("partial response body must write");
            watch_stalled_connection(
                stream,
                stopping,
                events,
                ServerEvent::StalledBody,
                ServerEvent::StalledBodyClosed,
            );
        }
        other if other.starts_with("/redirect-cross?target=") => {
            let target = &other["/redirect-cross?target=".len()..];
            write_redirect(&mut stream, 302, target);
        }
        other => panic!("unexpected test path: {other}"),
    }
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut content_length = None;
    loop {
        let read = stream.read(&mut buffer).expect("test request must read");
        assert_ne!(read, 0, "client closed before sending a request");
        request.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = find_bytes(&request, b"\r\n\r\n") {
            let body_start = header_end + 4;
            let length = *content_length.get_or_insert_with(|| parse_content_length(&request));
            if request.len() >= body_start + length {
                return request;
            }
        }
    }
}

fn parse_content_length(request: &[u8]) -> usize {
    String::from_utf8_lossy(request)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("length must parse"))
        })
        .unwrap_or(0)
}

fn request_body(request: &[u8]) -> &[u8] {
    let start = find_bytes(request, b"\r\n\r\n").expect("headers must terminate") + 4;
    &request[start..]
}

fn request_has_header(request: &[u8], expected: &str) -> bool {
    String::from_utf8_lossy(request).lines().any(|line| {
        line.split_once(':')
            .is_some_and(|(name, _value)| name.eq_ignore_ascii_case(expected))
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_response(stream: &mut TcpStream, status: u16, body: &[u8], headers: &[(&str, &str)]) {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    stream
        .write_all(response.as_bytes())
        .expect("response head must write");
    stream.write_all(body).expect("response body must write");
}

fn write_redirect(stream: &mut TcpStream, status: u16, location: &str) {
    let response = format!(
        "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(response.as_bytes())
        .expect("redirect must write");
}

fn watch_stalled_connection(
    mut stream: TcpStream,
    stopping: &AtomicBool,
    events: &mpsc::Sender<ServerEvent>,
    started: ServerEvent,
    closed: ServerEvent,
) {
    stream
        .set_nonblocking(true)
        .expect("stalled stream must become nonblocking");
    events.send(started).expect("test receiver must remain");
    let mut byte = [0_u8; 1];
    while !stopping.load(Ordering::Acquire) {
        match stream.read(&mut byte) {
            Ok(0) => {
                events.send(closed).expect("test receiver must remain");
                return;
            }
            Ok(_) => {}
            Err(error) if error.kind() == IoErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("stalled connection read failed: {error}"),
        }
    }
}

fn completion_response(completion: Completion) -> crate::Response {
    match completion {
        Completion::Completed(response) => response,
        Completion::Failed(error) => panic!("request unexpectedly failed: {error}"),
        Completion::Cancelled => panic!("request unexpectedly cancelled"),
    }
}

#[test]
fn curl_multi_runs_concurrent_get_status_and_post_requests() {
    let server = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let client = engine.client();
    let ok = client
        .submit(
            Request::get(server.url("/ok"))
                .build()
                .expect("GET must build"),
        )
        .expect("GET must submit");
    let missing = client
        .submit(
            Request::get(server.url("/not-found"))
                .build()
                .expect("404 request must build"),
        )
        .expect("404 request must submit");
    let echo = client
        .submit(
            Request::post(server.url("/echo"))
                .body(b"hello from HTTP arse".to_vec())
                .build()
                .expect("POST must build"),
        )
        .expect("POST must submit");

    let ok = completion_response(ok.wait());
    assert_eq!(ok.status(), 200);
    assert_eq!(ok.body(), b"hello");
    assert!(ok.headers().iter().any(|header| {
        header.name().eq_ignore_ascii_case("x-nbreq") && header.value() == b"yes"
    }));
    let missing = completion_response(missing.wait());
    assert_eq!(missing.status(), 404);
    assert_eq!(missing.body(), b"missing");
    let echo = completion_response(echo.wait());
    assert_eq!(echo.status(), 200);
    assert_eq!(echo.body(), b"hello from HTTP arse");
    engine.shutdown().expect("curl Engine must stop");
}

#[test]
fn busy_curl_poll_wakes_for_peer_submission_and_cancellation() {
    let server = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let client = engine.client();
    let stalled = client
        .submit(
            Request::get(server.url("/stall-body"))
                .build()
                .expect("stall request must build"),
        )
        .expect("stall request must submit");
    let stalled_handle = stalled.handle();
    server.expect_event(ServerEvent::StalledBody);
    thread::sleep(Duration::from_millis(20));

    let peer_started = Instant::now();
    let peer = client
        .submit(
            Request::get(server.url("/ok"))
                .build()
                .expect("peer must build"),
        )
        .expect("peer must submit");
    assert_eq!(completion_response(peer.wait()).status(), 200);
    assert!(peer_started.elapsed() < Duration::from_millis(500));

    stalled_handle
        .cancel()
        .expect("stalled request must cancel");
    assert!(matches!(stalled.wait(), Completion::Cancelled));
    let removal_latency =
        server.expect_event_within(ServerEvent::StalledBodyClosed, Duration::from_millis(500));
    eprintln!("curl stalled-body cancellation removal latency: {removal_latency:?}");
    engine.shutdown().expect("curl Engine must stop");
}

#[test]
fn curl_total_timeout_and_shutdown_interrupt_stalled_body() {
    let server = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let client = engine.client();
    let timed = client
        .submit(
            Request::get(server.url("/stall-headers"))
                .options(RequestOptions {
                    total_timeout: Some(Duration::from_millis(100)),
                    ..RequestOptions::default()
                })
                .build()
                .expect("timed request must build"),
        )
        .expect("timed request must submit");
    server.expect_event(ServerEvent::SlowHeaders);
    match timed.wait() {
        Completion::Failed(error) => assert_eq!(error.kind(), ErrorKind::Timeout),
        other => panic!("expected timeout failure, got {other:?}"),
    }
    server.expect_event(ServerEvent::SlowHeadersClosed);

    let pending = client
        .submit(
            Request::get(server.url("/stall-body"))
                .build()
                .expect("shutdown request must build"),
        )
        .expect("shutdown request must submit");
    server.expect_event(ServerEvent::StalledBody);
    let shutdown_started = Instant::now();
    engine.shutdown().expect("curl Engine must stop");
    assert!(shutdown_started.elapsed() < Duration::from_millis(500));
    assert!(matches!(pending.wait(), Completion::Cancelled));
    server.expect_event_within(ServerEvent::StalledBodyClosed, Duration::from_millis(500));
}

#[test]
fn curl_cancellation_latency_gate_covers_headers_and_body_stalls() {
    const GATE: Duration = Duration::from_millis(100);
    const TRIALS: usize = 10;

    let server = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let client = engine.client();
    let mut slow_header_max = Duration::ZERO;
    let mut stalled_body_max = Duration::ZERO;

    for _trial in 0..TRIALS {
        let pending = client
            .submit(
                Request::get(server.url("/stall-headers"))
                    .build()
                    .expect("slow-header request must build"),
            )
            .expect("slow-header request must submit");
        server.expect_event(ServerEvent::SlowHeaders);
        pending
            .handle()
            .cancel()
            .expect("slow-header request must cancel");
        assert!(matches!(pending.wait(), Completion::Cancelled));
        slow_header_max =
            slow_header_max.max(server.expect_event_within(ServerEvent::SlowHeadersClosed, GATE));

        let pending = client
            .submit(
                Request::get(server.url("/stall-body"))
                    .build()
                    .expect("stalled-body request must build"),
            )
            .expect("stalled-body request must submit");
        server.expect_event(ServerEvent::StalledBody);
        pending
            .handle()
            .cancel()
            .expect("stalled-body request must cancel");
        assert!(matches!(pending.wait(), Completion::Cancelled));
        stalled_body_max =
            stalled_body_max.max(server.expect_event_within(ServerEvent::StalledBodyClosed, GATE));
    }
    eprintln!(
        "curl cancellation gate={GATE:?} trials={TRIALS} slow-header-max={slow_header_max:?} stalled-body-max={stalled_body_max:?}"
    );
    engine.shutdown().expect("curl Engine must stop");
}

#[test]
fn active_spawned_curl_engine_owner_is_send_but_manual_curl_is_rejected() {
    let manual_error = testing::curl_engine(EngineConfig::manual())
        .err()
        .expect("manual curl must be rejected");
    assert_eq!(manual_error.kind(), ErrorKind::WrongMode);

    let server = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let client = engine.client();
    let pending = client
        .submit(
            Request::get(server.url("/stall-body"))
                .build()
                .expect("active request must build"),
        )
        .expect("active request must submit");
    server.expect_event(ServerEvent::StalledBody);

    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let moved_owner = thread::spawn(move || {
        shutdown_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("shutdown signal must arrive");
        engine.shutdown().expect("moved curl Engine must stop");
    });
    pending
        .handle()
        .cancel()
        .expect("active request must cancel through Client state");
    assert!(matches!(pending.wait(), Completion::Cancelled));
    server.expect_event_within(ServerEvent::StalledBodyClosed, Duration::from_millis(100));
    shutdown_tx.send(()).expect("owner signal must send");
    moved_owner.join().expect("moved Engine owner must join");
}

#[test]
fn curl_runtime_capabilities_are_recordable() {
    let version = curl::Version::get();
    eprintln!(
        "libcurl={} host={} ssl={:?} libz={:?} async_dns={} ipv6={} vendored={}",
        version.version(),
        version.host(),
        version.ssl_version(),
        version.libz_version(),
        version.feature_async_dns(),
        version.feature_ipv6(),
        version.vendored(),
    );
    assert!(version.version_num() >= 0x07_44_00);
    if cfg!(feature = "curl-vendored") {
        assert!(version.feature_ssl());
    }
    if std::env::var_os("NBREQ_EXPECT_DYNAMIC_CURL").is_some() {
        assert_eq!(version.version(), "8.21.0");
        assert_eq!(version.ssl_version(), Some("Schannel"));
        assert_eq!(version.libz_version(), None);
        assert!(!version.vendored());
    }
}

#[test]
fn curl_redirects_follow_the_portable_method_and_credential_rules() {
    let source = LocalServer::start();
    let target = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let client = engine.client();

    let post_302 = client
        .execute(
            Request::post(source.url("/redirect-302-post"))
                .body(b"do not rewrite me".to_vec())
                .build()
                .expect("302 POST must build"),
        )
        .expect("302 POST must complete");
    assert_eq!(post_302.status(), 302);

    let post_303 = client
        .execute(
            Request::post(source.url("/redirect-303-post"))
                .body(b"drop me".to_vec())
                .build()
                .expect("303 POST must build"),
        )
        .expect("303 POST redirect must complete");
    assert_eq!(post_303.body(), b"method=GET;body=0");

    let post_307 = client
        .execute(
            Request::post(source.url("/redirect-307-post"))
                .body(b"replay me".to_vec())
                .build()
                .expect("307 POST must build"),
        )
        .expect("307 POST redirect must complete");
    assert_eq!(post_307.body(), b"replay me");

    let same_origin = client
        .execute(
            Request::get(source.url("/redirect-same"))
                .header("Authorization", "Basic definitely-not-a-real-secret")
                .header("Cookie", "pilot=yes")
                .build()
                .expect("same-origin redirect must build"),
        )
        .expect("same-origin redirect must complete");
    assert_eq!(same_origin.body(), b"authorization=true;cookie=true");

    let cross_path = format!("/redirect-cross?target={}", target.url("/inspect-auth"));
    let cross_origin = client
        .execute(
            Request::get(source.url(&cross_path))
                .header("Authorization", "Basic definitely-not-a-real-secret")
                .header("Cookie", "pilot=yes")
                .build()
                .expect("cross-origin redirect must build"),
        )
        .expect("cross-origin redirect must complete");
    assert_eq!(cross_origin.body(), b"authorization=false;cookie=false");

    let looped = client
        .submit(
            Request::get(source.url("/redirect-loop"))
                .options(RequestOptions {
                    redirect_limit: 2,
                    tls_verification: TlsVerification::Verify,
                    ..RequestOptions::default()
                })
                .build()
                .expect("redirect loop must build"),
        )
        .expect("redirect loop must submit");
    match looped.wait() {
        Completion::Failed(error) => assert_eq!(error.kind(), ErrorKind::Redirect),
        other => panic!("expected redirect failure, got {other:?}"),
    }

    engine.shutdown().expect("curl Engine must stop");
}
