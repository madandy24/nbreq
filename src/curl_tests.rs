use std::io::{ErrorKind as IoErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    date_time_ymd,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::testing;
use crate::{
    Completion, EngineConfig, ErrorKind, LimitKind, Method, Request, RequestOptions, TimeoutKind,
    TlsVerification, TransportStage,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerEvent {
    TlsHandshake,
    TlsHandshakeClosed,
    SlowHeaders,
    SlowHeadersClosed,
    StalledBody,
    StalledBodyClosed,
}

struct TlsHandshakeStallServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    accepted: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    events: mpsc::Receiver<ServerEvent>,
}

impl TlsHandshakeStallServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("TLS stall listener must bind");
        let address = listener.local_addr().expect("TLS stall listener address");
        let stopping = Arc::new(AtomicBool::new(false));
        let listener_stopping = Arc::clone(&stopping);
        let accepted = Arc::new(AtomicBool::new(false));
        let listener_accepted = Arc::clone(&accepted);
        let (events_tx, events) = mpsc::channel();
        let listener_thread = thread::spawn(move || {
            let (stream, _peer) = listener.accept().expect("TLS stall must accept");
            listener_accepted.store(true, Ordering::Release);
            if !listener_stopping.load(Ordering::Acquire) {
                watch_stalled_tls_handshake(stream, &listener_stopping, &events_tx);
            }
        });
        Self {
            address,
            stopping,
            accepted,
            listener: Some(listener_thread),
            events,
        }
    }

    fn url(&self) -> String {
        format!("https://localhost:{}/", self.address.port())
    }

    fn expect_event(&self, expected: ServerEvent, timeout: Duration) -> Duration {
        let started = Instant::now();
        assert_eq!(
            self.events
                .recv_timeout(timeout)
                .expect("TLS stall event must arrive"),
            expected
        );
        started.elapsed()
    }
}

impl Drop for TlsHandshakeStallServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if !self.accepted.load(Ordering::Acquire) {
            let _wake_listener = TcpStream::connect(self.address);
        }
        if let Some(listener) = self.listener.take() {
            listener.join().expect("TLS stall listener must join");
        }
    }
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

struct TestIdentity {
    chain: Vec<CertificateDer<'static>>,
    key: Vec<u8>,
    ca_pem: Vec<u8>,
}

impl TestIdentity {
    fn localhost(expired: bool) -> Self {
        let key = KeyPair::generate().expect("test TLS key must generate");
        let mut params = CertificateParams::new(vec!["localhost".to_owned()]).expect("TLS params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::CrlSign,
        ];
        if expired {
            params.not_before = date_time_ymd(2010, 1, 1);
            params.not_after = date_time_ymd(2011, 1, 1);
        }
        let cert = params.self_signed(&key).expect("test TLS cert must sign");
        Self {
            chain: vec![cert.der().clone()],
            key: key.serialize_der(),
            ca_pem: cert.pem().into_bytes(),
        }
    }
}

struct TlsServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
}

impl TlsServer {
    fn start(identity: &TestIdentity) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("TLS test listener must bind");
        listener
            .set_nonblocking(true)
            .expect("TLS test listener must become nonblocking");
        let address = listener.local_addr().expect("TLS listener address");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("TLS protocol versions must configure")
            .with_no_client_auth()
            .with_single_cert(
                identity.chain.clone(),
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone())),
            )
            .expect("TLS identity must configure");
        let config = Arc::new(config);
        let stopping = Arc::new(AtomicBool::new(false));
        let listener_stopping = Arc::clone(&stopping);
        let listener_thread = thread::spawn(move || {
            while !listener_stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        if listener_stopping.load(Ordering::Acquire) {
                            break;
                        }
                        stream
                            .set_nonblocking(false)
                            .expect("accepted TLS test stream must become blocking");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .expect("TLS test stream read timeout must configure");
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .expect("TLS test stream write timeout must configure");
                        let connection =
                            ServerConnection::new(Arc::clone(&config)).expect("TLS server state");
                        let mut stream = StreamOwned::new(connection, stream);
                        if read_request_result(&mut stream).is_ok() {
                            write_response(&mut stream, 200, b"secure", &[]);
                        }
                    }
                    Err(error) if error.kind() == IoErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("TLS test listener failed: {error}"),
                }
            }
        });
        Self {
            address,
            stopping,
            listener: Some(listener_thread),
        }
    }

    fn url(&self, host: &str) -> String {
        format!("https://{host}:{}/", self.address.port())
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _wake_listener = TcpStream::connect(self.address);
        if let Some(listener) = self.listener.take() {
            listener.join().expect("TLS test listener must join");
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
        "/inspect-content-type" => {
            let content_type = request_has_header(&request, "content-type");
            let body = format!("method={method};content-type={content_type}");
            write_response(&mut stream, 200, body.as_bytes(), &[]);
        }
        "/informational" => {
            stream
                .write_all(b"HTTP/1.1 100 Continue\r\nX-Interim: yes\r\n\r\n")
                .expect("informational response must write");
            write_response(&mut stream, 200, b"final", &[]);
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

fn read_request(stream: &mut impl Read) -> Vec<u8> {
    read_request_result(stream).expect("test request must read")
}

fn read_request_result(stream: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut content_length = None;
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(std::io::Error::new(
                IoErrorKind::UnexpectedEof,
                "client closed before sending a request",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = find_bytes(&request, b"\r\n\r\n") {
            let body_start = header_end + 4;
            let length = *content_length.get_or_insert_with(|| parse_content_length(&request));
            if request.len() >= body_start + length {
                return Ok(request);
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

fn write_response(stream: &mut impl Write, status: u16, body: &[u8], headers: &[(&str, &str)]) {
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

fn write_redirect(stream: &mut impl Write, status: u16, location: &str) {
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

fn watch_stalled_tls_handshake(
    mut stream: TcpStream,
    stopping: &AtomicBool,
    events: &mpsc::Sender<ServerEvent>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("TLS stall read timeout must configure");
    let mut client_hello = [0_u8; 4096];
    let read = stream
        .read(&mut client_hello)
        .expect("TLS stall must receive ClientHello bytes");
    assert_ne!(
        read, 0,
        "TLS client closed before sending ClientHello bytes"
    );
    watch_stalled_connection(
        stream,
        stopping,
        events,
        ServerEvent::TlsHandshake,
        ServerEvent::TlsHandshakeClosed,
    );
}

fn completion_response(completion: Completion) -> crate::Response {
    match completion {
        Completion::Completed(response) => response,
        Completion::Failed(error) => panic!("request unexpectedly failed: {error}"),
        Completion::Cancelled => panic!("request unexpectedly cancelled"),
    }
}

fn tls_request(url: String, verification: TlsVerification) -> Request {
    Request::get(url)
        .options(RequestOptions {
            connect_timeout: Some(Duration::from_secs(1)),
            total_timeout: Some(Duration::from_secs(2)),
            tls_verification: verification,
            ..RequestOptions::default()
        })
        .build()
        .expect("TLS request must build")
}

fn assert_tls_failure(completion: Completion) {
    match completion {
        Completion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Transport);
            assert_eq!(error.transport_stage(), Some(TransportStage::Tls));
        }
        other => panic!("expected TLS failure, got {other:?}"),
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
fn curl_tls_fixture_proves_trust_name_expiry_and_explicit_no_verify() {
    if !curl::Version::get().feature_ssl() {
        eprintln!("skipping TLS fixture because the selected curl pilot has no TLS backend");
        return;
    }
    let valid_identity = TestIdentity::localhost(false);
    let valid_server = TlsServer::start(&valid_identity);

    let trusted =
        testing::curl_engine_with_test_ca(EngineConfig::spawned(), valid_identity.ca_pem.clone())
            .expect("trusted curl Engine must construct");
    let response = trusted
        .client()
        .execute(tls_request(
            valid_server.url("localhost"),
            TlsVerification::Verify,
        ))
        .expect("locally trusted TLS request must complete");
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"secure");
    trusted.shutdown().expect("trusted Engine must stop");

    let wrong_host =
        testing::curl_engine_with_test_ca(EngineConfig::spawned(), valid_identity.ca_pem.clone())
            .expect("wrong-host curl Engine must construct");
    let completion = wrong_host
        .client()
        .submit(tls_request(
            valid_server.url("127.0.0.1"),
            TlsVerification::Verify,
        ))
        .expect("wrong-host request must submit")
        .wait();
    assert_tls_failure(completion);
    wrong_host.shutdown().expect("wrong-host Engine must stop");

    let unknown_root = testing::curl_engine(EngineConfig::spawned())
        .expect("unknown-root curl Engine must construct");
    let completion = unknown_root
        .client()
        .submit(tls_request(
            valid_server.url("localhost"),
            TlsVerification::Verify,
        ))
        .expect("unknown-root request must submit")
        .wait();
    assert_tls_failure(completion);
    unknown_root
        .shutdown()
        .expect("unknown-root Engine must stop");

    let no_verify = testing::curl_engine(EngineConfig::spawned())
        .expect("no-verify curl Engine must construct");
    let response = no_verify
        .client()
        .execute(tls_request(
            valid_server.url("localhost"),
            TlsVerification::DangerouslyDisableCertificateVerification,
        ))
        .expect("explicitly unverified TLS request must complete");
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"secure");
    no_verify.shutdown().expect("no-verify Engine must stop");

    let expired_identity = TestIdentity::localhost(true);
    let expired_server = TlsServer::start(&expired_identity);
    let expired =
        testing::curl_engine_with_test_ca(EngineConfig::spawned(), expired_identity.ca_pem.clone())
            .expect("expired-cert curl Engine must construct");
    let completion = expired
        .client()
        .submit(tls_request(
            expired_server.url("localhost"),
            TlsVerification::Verify,
        ))
        .expect("expired-cert request must submit")
        .wait();
    assert_tls_failure(completion);
    expired.shutdown().expect("expired-cert Engine must stop");
}

#[test]
fn curl_cancellation_latency_gate_covers_tls_handshake() {
    if !curl::Version::get().feature_ssl() {
        eprintln!("skipping TLS cancellation fixture because curl has no TLS backend");
        return;
    }
    const GATE: Duration = Duration::from_millis(100);
    const TRIALS: usize = 10;

    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let client = engine.client();
    let mut max_removal = Duration::ZERO;
    for _trial in 0..TRIALS {
        let server = TlsHandshakeStallServer::start();
        let pending = client
            .submit(tls_request(server.url(), TlsVerification::Verify))
            .expect("TLS stall request must submit");
        server.expect_event(ServerEvent::TlsHandshake, Duration::from_secs(2));
        pending
            .handle()
            .cancel()
            .expect("TLS handshake request must cancel");
        assert!(matches!(pending.wait(), Completion::Cancelled));
        max_removal = max_removal.max(server.expect_event(ServerEvent::TlsHandshakeClosed, GATE));
    }
    eprintln!(
        "curl TLS-handshake cancellation gate={GATE:?} trials={TRIALS} socket-release-max={max_removal:?}"
    );
    engine.shutdown().expect("curl Engine must stop");
}

#[test]
fn curl_enforces_request_and_response_buffer_limits_before_growth() {
    let request_engine = testing::curl_engine(
        EngineConfig::spawned()
            .with_max_request_body_bytes(4)
            .with_max_header_bytes(8)
            .with_max_header_count(1),
    )
    .expect("curl Engine must construct");
    let request_client = request_engine.client();

    let body_error = request_client
        .submit(
            Request::post("http://example.invalid/")
                .body(b"12345".to_vec())
                .build()
                .expect("request must build"),
        )
        .expect_err("oversized request body must be rejected");
    assert_eq!(body_error.kind(), ErrorKind::Limit);
    assert_eq!(body_error.limit_kind(), Some(LimitKind::RequestBodyBytes));

    let count_error = request_client
        .submit(
            Request::get("http://example.invalid/")
                .header("X-One", "1")
                .header("X-Two", "2")
                .build()
                .expect("request must build"),
        )
        .expect_err("too many request headers must be rejected");
    assert_eq!(
        count_error.limit_kind(),
        Some(LimitKind::RequestHeaderCount)
    );

    let bytes_error = request_client
        .submit(
            Request::get("http://example.invalid/")
                .header("X-Long", "12345678")
                .build()
                .expect("request must build"),
        )
        .expect_err("oversized request headers must be rejected");
    assert_eq!(
        bytes_error.limit_kind(),
        Some(LimitKind::RequestHeaderBytes)
    );
    request_engine.shutdown().expect("curl Engine must stop");

    let server = LocalServer::start();
    let body_engine = testing::curl_engine(EngineConfig::spawned().with_max_response_body_bytes(4))
        .expect("curl Engine must construct");
    let body = body_engine
        .client()
        .submit(
            Request::get(server.url("/ok"))
                .build()
                .expect("request must build"),
        )
        .expect("request must submit")
        .wait();
    match body {
        Completion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Limit);
            assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
        }
        other => panic!("expected response body limit failure, got {other:?}"),
    }
    body_engine.shutdown().expect("curl Engine must stop");

    let header_engine = testing::curl_engine(EngineConfig::spawned().with_max_header_count(1))
        .expect("curl Engine must construct");
    let headers = header_engine
        .client()
        .submit(
            Request::get(server.url("/ok"))
                .build()
                .expect("request must build"),
        )
        .expect("request must submit")
        .wait();
    match headers {
        Completion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Limit);
            assert_eq!(error.limit_kind(), Some(LimitKind::ResponseHeaderCount));
        }
        other => panic!("expected response header count failure, got {other:?}"),
    }
    header_engine.shutdown().expect("curl Engine must stop");

    let header_bytes_engine =
        testing::curl_engine(EngineConfig::spawned().with_max_header_bytes(16))
            .expect("curl Engine must construct");
    let headers = header_bytes_engine
        .client()
        .submit(
            Request::get(server.url("/ok"))
                .build()
                .expect("request must build"),
        )
        .expect("request must submit")
        .wait();
    match headers {
        Completion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Limit);
            assert_eq!(error.limit_kind(), Some(LimitKind::ResponseHeaderBytes));
        }
        other => panic!("expected response header byte failure, got {other:?}"),
    }
    header_bytes_engine
        .shutdown()
        .expect("curl Engine must stop");
}

#[test]
fn curl_inactivity_timeout_uses_subsecond_progress_semantics() {
    let server = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let pending = engine
        .client()
        .submit(
            Request::get(server.url("/stall-body"))
                .options(RequestOptions {
                    inactivity_timeout: Some(Duration::from_millis(100)),
                    total_timeout: Some(Duration::from_secs(2)),
                    ..RequestOptions::default()
                })
                .build()
                .expect("request must build"),
        )
        .expect("request must submit");
    server.expect_event(ServerEvent::StalledBody);
    let started = Instant::now();
    match pending.wait() {
        Completion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Timeout);
            assert_eq!(error.timeout_kind(), Some(TimeoutKind::Inactivity));
        }
        other => panic!("expected inactivity timeout, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "subsecond inactivity timeout exceeded its bounded pilot resolution"
    );
    server.expect_event(ServerEvent::StalledBodyClosed);
    engine.shutdown().expect("curl Engine must stop");
}

#[test]
fn curl_custom_method_body_does_not_invent_a_content_type() {
    let server = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned()).expect("curl Engine must construct");
    let response = engine
        .client()
        .submit(
            Request::builder(Method::Patch, server.url("/inspect-content-type"))
                .body(b"payload".to_vec())
                .build()
                .expect("request must build"),
        )
        .expect("request must submit")
        .wait();
    let response = completion_response(response);
    assert_eq!(response.body(), b"method=PATCH;content-type=false");
    engine.shutdown().expect("curl Engine must stop");
}

#[test]
fn curl_header_budget_resets_for_each_informational_or_final_head() {
    let server = LocalServer::start();
    let engine = testing::curl_engine(EngineConfig::spawned().with_max_header_count(2))
        .expect("curl Engine must construct");
    let response = engine
        .client()
        .submit(
            Request::get(server.url("/informational"))
                .build()
                .expect("request must build"),
        )
        .expect("request must submit")
        .wait();
    let response = completion_response(response);
    assert_eq!(response.body(), b"final");
    assert_eq!(response.headers().len(), 2);
    assert!(
        response
            .headers()
            .iter()
            .all(|header| !header.name().eq_ignore_ascii_case("x-interim"))
    );
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
                    connect_timeout: Some(Duration::from_secs(1)),
                    total_timeout: Some(Duration::from_millis(100)),
                    ..RequestOptions::default()
                })
                .build()
                .expect("timed request must build"),
        )
        .expect("timed request must submit");
    server.expect_event(ServerEvent::SlowHeaders);
    match timed.wait() {
        Completion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Timeout);
            assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
        }
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
    if cfg!(feature = "curl-pilot-vendored") {
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
