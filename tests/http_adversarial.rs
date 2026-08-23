#![cfg(any(feature = "curl-pilot", feature = "native"))]

use std::io::{ErrorKind as IoErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nbreq::{
    Completion, Engine, EngineConfig, ErrorKind, ExecuteError, HttpBackend, LimitKind, Request,
    TimeoutKind, TlsVerification, TransportStage,
};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use socket2::SockRef;

#[derive(Clone, Copy)]
enum Script {
    Bytes(&'static [u8]),
    Fragmented(&'static [u8]),
    ResetDuringUpload,
    TwoKeepAliveResponses,
    StallAfterHead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerEvent {
    RequestSeen,
    PeerClosed,
}

struct ScriptedServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    events: Receiver<ServerEvent>,
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
        let (event_sender, events) = mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker_stopping.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _peer)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("accepted lab stream must become blocking");
                        // The upload-reset case deliberately allocates and serializes a 64 MiB
                        // body. A small CI host can spend more than two seconds there under
                        // repeated stress, after the server has accepted but before request bytes
                        // arrive. Do not let the fixture manufacture a connect-stage reset before
                        // it has observed the head/body progress this case claims to test.
                        let read_timeout = match script {
                            Script::ResetDuringUpload => Duration::from_secs(10),
                            _ => Duration::from_secs(2),
                        };
                        stream
                            .set_read_timeout(Some(read_timeout))
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
                                let _ = event_sender.send(ServerEvent::RequestSeen);
                                stream.write_all(response).expect("lab response must write");
                            }
                            Script::Fragmented(response) => {
                                read_request_head(&mut stream);
                                let _ = event_sender.send(ServerEvent::RequestSeen);
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
                                let body_bytes = read_request_head(&mut stream);
                                let _ = event_sender.send(ServerEvent::RequestSeen);
                                if body_bytes == 0 {
                                    let mut body_byte = [0_u8; 1];
                                    stream
                                        .read_exact(&mut body_byte)
                                        .expect("lab server must observe upload data before reset");
                                }
                            }
                            Script::TwoKeepAliveResponses => {
                                for (body, connection) in [
                                    (b"one".as_slice(), "keep-alive"),
                                    (b"two".as_slice(), "close"),
                                ] {
                                    read_request_head(&mut stream);
                                    let _ = event_sender.send(ServerEvent::RequestSeen);
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
                            Script::StallAfterHead => {
                                read_request_head(&mut stream);
                                stream
                                    .write_all(
                                        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n",
                                    )
                                    .expect("stalled response head must write");
                                stream.flush().expect("stalled response head must flush");
                                let _ = event_sender.send(ServerEvent::RequestSeen);
                                wait_for_peer_close(&mut stream, &worker_stopping);
                                let _ = event_sender.send(ServerEvent::PeerClosed);
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
            events,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/", self.address)
    }

    fn wait_for_request(&self) {
        self.wait_for_event(ServerEvent::RequestSeen, "request arrival");
    }

    fn wait_for_peer_close(&self) {
        self.wait_for_event(ServerEvent::PeerClosed, "peer close");
    }

    fn wait_for_event(&self, expected: ServerEvent, description: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for {description}");
            let event = self
                .events
                .recv_timeout(remaining)
                .unwrap_or_else(|error| panic!("failed waiting for {description}: {error}"));
            if event == expected {
                return;
            }
        }
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

enum RedirectScenario {
    Post302Stops,
    Post303BecomesGet,
    Post307Replays,
    SameOriginCredentials,
    RedirectTo(String),
    InspectCredentials,
    Loop,
}

struct RedirectServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl RedirectServer {
    fn start(scenario: RedirectScenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("redirect listener must bind");
        listener
            .set_nonblocking(true)
            .expect("redirect listener must become nonblocking");
        let address = listener.local_addr().expect("redirect listener address");
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            match scenario {
                RedirectScenario::Post302Stops => {
                    let (mut stream, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut stream);
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.body, b"payload");
                    write_redirect(&mut stream, 302, "/must-not-follow");
                }
                RedirectScenario::Post303BecomesGet => {
                    let (mut first, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut first);
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.body, b"payload");
                    write_redirect(&mut first, 303, "/final");
                    drop(first);

                    let (mut second, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut second);
                    assert_eq!(request.path, "/final");
                    let body = format!("method={};body={}", request.method, request.body.len());
                    write_ok(&mut second, body.as_bytes());
                }
                RedirectScenario::Post307Replays => {
                    let (mut first, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut first);
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.body, b"payload");
                    write_redirect(&mut first, 307, "/final");
                    drop(first);

                    let (mut second, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut second);
                    assert_eq!(request.path, "/final");
                    let body = format!(
                        "method={};body={}",
                        request.method,
                        String::from_utf8_lossy(&request.body)
                    );
                    write_ok(&mut second, body.as_bytes());
                }
                RedirectScenario::SameOriginCredentials => {
                    let (mut first, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut first);
                    assert!(request.authorization);
                    assert!(request.cookie);
                    write_redirect(&mut first, 302, "/final");
                    drop(first);

                    let (mut second, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut second);
                    let body = format!(
                        "authorization={};cookie={}",
                        request.authorization, request.cookie
                    );
                    write_ok(&mut second, body.as_bytes());
                }
                RedirectScenario::RedirectTo(target) => {
                    let (mut stream, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut stream);
                    assert!(request.authorization);
                    assert!(request.cookie);
                    write_redirect(&mut stream, 302, &target);
                }
                RedirectScenario::InspectCredentials => {
                    let (mut stream, _) =
                        accept_redirect_stream(&listener, &worker_stopping, deadline);
                    let request = read_request(&mut stream);
                    let body = format!(
                        "authorization={};cookie={}",
                        request.authorization, request.cookie
                    );
                    write_ok(&mut stream, body.as_bytes());
                }
                RedirectScenario::Loop => {
                    for _ in 0..3 {
                        let (mut stream, _) =
                            accept_redirect_stream(&listener, &worker_stopping, deadline);
                        let request = read_request(&mut stream);
                        assert_eq!(request.method, "GET");
                        write_redirect(&mut stream, 302, "/loop");
                    }
                }
            }
        });
        Self {
            address,
            stopping,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/start", self.address)
    }

    fn final_url(&self) -> String {
        format!("http://{}/final", self.address)
    }
}

impl Drop for RedirectServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let joined = worker.join();
            if !thread::panicking() {
                joined.expect("redirect server must join");
            }
        }
    }
}

struct TlsIdentity {
    chain: Vec<CertificateDer<'static>>,
    key: Vec<u8>,
}

impl TlsIdentity {
    fn localhost() -> Self {
        let key = KeyPair::generate().expect("test TLS key must generate");
        let mut params = CertificateParams::new(vec!["localhost".to_owned()])
            .expect("test TLS parameters must build");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::CrlSign,
        ];
        let cert = params
            .self_signed(&key)
            .expect("test TLS certificate must sign");
        Self {
            chain: vec![cert.der().clone()],
            key: key.serialize_der(),
        }
    }
}

struct TlsServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TlsServer {
    fn start(identity: &TlsIdentity) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("TLS listener must bind");
        listener
            .set_nonblocking(true)
            .expect("TLS listener must become nonblocking");
        let address = listener.local_addr().expect("TLS listener address");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("TLS versions must configure")
            .with_no_client_auth()
            .with_single_cert(
                identity.chain.clone(),
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone())),
            )
            .expect("TLS identity must configure");
        let config = Arc::new(config);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::spawn(move || {
            while !worker_stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if worker_stopping.load(Ordering::Acquire) {
                            return;
                        }
                        stream
                            .set_nonblocking(false)
                            .expect("TLS stream must become blocking");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .expect("TLS read timeout must configure");
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .expect("TLS write timeout must configure");
                        let connection = ServerConnection::new(Arc::clone(&config))
                            .expect("TLS server connection must construct");
                        let mut stream = StreamOwned::new(connection, stream);
                        if read_request_result(&mut stream).is_ok() {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecure",
                            );
                            let _ = stream.flush();
                        }
                    }
                    Err(error) if error.kind() == IoErrorKind::WouldBlock => thread::yield_now(),
                    Err(error) => panic!("TLS listener failed: {error}"),
                }
            }
        });
        Self {
            address,
            stopping,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("https://localhost:{}/", self.address.port())
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let joined = worker.join();
            if !thread::panicking() {
                joined.expect("TLS server must join");
            }
        }
    }
}

struct ObservedRequest {
    method: String,
    path: String,
    body: Vec<u8>,
    authorization: bool,
    cookie: bool,
}

fn accept_redirect_stream(
    listener: &TcpListener,
    stopping: &AtomicBool,
    deadline: Instant,
) -> (TcpStream, SocketAddr) {
    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                stream
                    .set_nonblocking(false)
                    .expect("redirect stream must become blocking");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("redirect read timeout must configure");
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .expect("redirect write timeout must configure");
                stream
                    .set_nodelay(true)
                    .expect("redirect TCP_NODELAY must configure");
                return (stream, peer);
            }
            Err(error) if error.kind() == IoErrorKind::WouldBlock => {
                assert!(
                    !stopping.load(Ordering::Acquire),
                    "redirect server stopped before the expected request"
                );
                assert!(
                    Instant::now() < deadline,
                    "redirect server timed out waiting for a request"
                );
                thread::yield_now();
            }
            Err(error) => panic!("redirect listener failed: {error}"),
        }
    }
}

fn read_request(stream: &mut TcpStream) -> ObservedRequest {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 512];
    let head_end = loop {
        if let Some(offset) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
        let read = stream
            .read(&mut buffer)
            .expect("redirect request head must read");
        assert_ne!(read, 0, "client closed before redirect request head");
        received.extend_from_slice(&buffer[..read]);
    };
    let head = std::str::from_utf8(&received[..head_end]).expect("request head must be UTF-8");
    let mut lines = head.split("\r\n");
    let mut request_line = lines
        .next()
        .expect("request line must exist")
        .split_whitespace();
    let method = request_line
        .next()
        .expect("request method must exist")
        .to_owned();
    let path = request_line
        .next()
        .expect("request path must exist")
        .to_owned();
    let headers: Vec<_> = lines.filter_map(|line| line.split_once(':')).collect();
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        .map(|(_, value)| {
            value
                .trim()
                .parse::<usize>()
                .expect("request Content-Length must be numeric")
        })
        .unwrap_or(0);
    let authorization = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("Authorization"));
    let cookie = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("Cookie"));
    let mut body = received.split_off(head_end);
    while body.len() < content_length {
        let read = stream
            .read(&mut buffer)
            .expect("redirect request body must read");
        assert_ne!(read, 0, "client closed before redirect request body");
        body.extend_from_slice(&buffer[..read]);
    }
    body.truncate(content_length);
    ObservedRequest {
        method,
        path,
        body,
        authorization,
        cookie,
    }
}

fn write_redirect(stream: &mut TcpStream, status: u16, location: &str) {
    let response = format!(
        "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(response.as_bytes())
        .expect("redirect response must write");
}

fn write_ok(stream: &mut TcpStream, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .expect("final response head must write");
    stream
        .write_all(body)
        .expect("final response body must write");
}

fn wait_for_peer_close(stream: &mut TcpStream, stopping: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffer = [0_u8; 64];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    IoErrorKind::ConnectionReset
                        | IoErrorKind::ConnectionAborted
                        | IoErrorKind::BrokenPipe
                ) =>
            {
                return;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    IoErrorKind::WouldBlock | IoErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("failed while waiting for client close: {error}"),
        }
        if stopping.load(Ordering::Acquire) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "client did not close the stalled connection"
        );
    }
}

fn read_request_head(stream: &mut TcpStream) -> usize {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 512];
    loop {
        if let Some(head_end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            return received.len() - (head_end + 4);
        }
        let read = stream
            .read(&mut buffer)
            .expect("lab request head must read");
        assert_ne!(read, 0, "client closed before sending a request head");
        received.extend_from_slice(&buffer[..read]);
    }
}

fn read_request_result(stream: &mut impl Read) -> std::io::Result<()> {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 512];
    loop {
        if received.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(());
        }
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(std::io::Error::new(
                IoErrorKind::UnexpectedEof,
                "client closed before sending a request head",
            ));
        }
        received.extend_from_slice(&buffer[..read]);
    }
}

fn test_backends() -> Vec<(&'static str, HttpBackend)> {
    #[cfg(all(feature = "native", feature = "curl-pilot"))]
    {
        vec![("native", HttpBackend::Native), ("curl", HttpBackend::Curl)]
    }
    #[cfg(all(feature = "native", not(feature = "curl-pilot")))]
    {
        vec![("native", HttpBackend::Native)]
    }
    #[cfg(all(feature = "curl-pilot", not(feature = "native")))]
    {
        vec![("curl", HttpBackend::Curl)]
    }
}

fn test_engine(config: EngineConfig, backend: HttpBackend) -> Engine {
    Engine::builder()
        .config(config)
        .http_backend(backend)
        .build()
        .expect("selected lab Engine must construct")
}

fn backend_has_tls(backend: HttpBackend) -> bool {
    if backend == HttpBackend::Curl {
        #[cfg(feature = "curl-pilot")]
        {
            return curl::Version::get().feature_ssl();
        }
        #[cfg(not(feature = "curl-pilot"))]
        unreachable!("curl cannot be selected when its feature is absent");
    }
    true
}

fn execute(
    backend: HttpBackend,
    config: EngineConfig,
    script: Script,
) -> Result<nbreq::Response, ExecuteError> {
    let server = ScriptedServer::start(script);
    let engine = test_engine(config, backend);
    let result = engine.client().execute(
        Request::get(server.url())
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("lab request must build"),
    );
    engine.shutdown().expect("lab Engine must stop");
    result
}

fn assert_transport(
    backend_name: &str,
    backend: HttpBackend,
    case: &str,
    script: Script,
    expected: TransportStage,
) {
    match execute(backend, EngineConfig::spawned(), script)
        .expect_err("adversarial response must fail")
    {
        ExecuteError::Failed(error) => {
            assert_eq!(
                error.kind(),
                ErrorKind::Transport,
                "{backend_name}/{case}: {error}"
            );
            assert_eq!(
                error.transport_stage(),
                Some(expected),
                "{backend_name}/{case}: {error}"
            );
        }
        other => {
            panic!("{backend_name}/{case}: expected terminal transport failure, got {other:?}")
        }
    }
}

#[test]
fn fragmented_and_chunked_responses_complete_through_the_public_api() {
    for (backend_name, backend) in test_backends() {
        let fragmented = execute(
            backend,
            EngineConfig::spawned(),
            Script::Fragmented(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            ),
        )
        .unwrap_or_else(|error| panic!("{backend_name}: fragmented response failed: {error}"));
        assert_eq!(fragmented.status(), 200, "{backend_name}");
        assert_eq!(fragmented.body(), b"hello", "{backend_name}");

        let chunked = execute(
            backend,
            EngineConfig::spawned(),
            Script::Bytes(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5;lab=yes\r\nhello\r\n0\r\nX-Lab-Trailer: yes\r\n\r\n",
            ),
        )
        .unwrap_or_else(|error| panic!("{backend_name}: chunked response failed: {error}"));
        assert_eq!(chunked.status(), 200, "{backend_name}");
        assert_eq!(chunked.body(), b"hello", "{backend_name}");

        let repeated_length = execute(
            backend,
            EngineConfig::spawned(),
            Script::Bytes(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            ),
        )
        .unwrap_or_else(|error| {
            panic!("{backend_name}: identical repeated lengths failed: {error}")
        });
        assert_eq!(repeated_length.body(), b"hello", "{backend_name}");
    }
}

#[test]
fn sequential_requests_reuse_one_http11_connection() {
    for (backend_name, backend) in test_backends() {
        let server = ScriptedServer::start(Script::TwoKeepAliveResponses);
        let engine = test_engine(EngineConfig::spawned(), backend);
        let client = engine.client();
        for expected in [b"one".as_slice(), b"two".as_slice()] {
            let response = client
                .execute(
                    Request::get(server.url())
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("reuse request must build"),
                )
                .unwrap_or_else(|error| panic!("{backend_name}: reuse failed: {error}"));
            assert_eq!(response.body(), expected, "{backend_name}");
        }
        engine
            .shutdown()
            .unwrap_or_else(|error| panic!("{backend_name}: lab Engine failed to stop: {error}"));
    }
}

#[test]
fn redirects_share_the_portable_method_body_and_hop_rules() {
    for (backend_name, backend) in test_backends() {
        let engine = test_engine(EngineConfig::spawned(), backend);
        let client = engine.client();

        let post_302 = RedirectServer::start(RedirectScenario::Post302Stops);
        let response = client
            .execute(
                Request::post(post_302.url())
                    .body(b"payload".to_vec())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("302 request must build"),
            )
            .unwrap_or_else(|error| panic!("{backend_name}: 302 request failed: {error}"));
        assert_eq!(response.status(), 302, "{backend_name}: POST 302");

        let post_303 = RedirectServer::start(RedirectScenario::Post303BecomesGet);
        let response = client
            .execute(
                Request::post(post_303.url())
                    .body(b"payload".to_vec())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("303 request must build"),
            )
            .unwrap_or_else(|error| panic!("{backend_name}: 303 request failed: {error}"));
        assert_eq!(response.body(), b"method=GET;body=0", "{backend_name}");

        let post_307 = RedirectServer::start(RedirectScenario::Post307Replays);
        let response = client
            .execute(
                Request::post(post_307.url())
                    .body(b"payload".to_vec())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("307 request must build"),
            )
            .unwrap_or_else(|error| panic!("{backend_name}: 307 request failed: {error}"));
        assert_eq!(
            response.body(),
            b"method=POST;body=payload",
            "{backend_name}"
        );

        let same_origin = RedirectServer::start(RedirectScenario::SameOriginCredentials);
        let response = client
            .execute(
                Request::get(same_origin.url())
                    .header("Authorization", "Basic parity-fixture")
                    .header("Cookie", "parity=yes")
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("same-origin credential request must build"),
            )
            .unwrap_or_else(|error| panic!("{backend_name}: same-origin redirect failed: {error}"));
        assert_eq!(
            response.body(),
            b"authorization=true;cookie=true",
            "{backend_name}"
        );

        let cross_target = RedirectServer::start(RedirectScenario::InspectCredentials);
        let cross_source =
            RedirectServer::start(RedirectScenario::RedirectTo(cross_target.final_url()));
        let response = client
            .execute(
                Request::get(cross_source.url())
                    .header("Authorization", "Basic parity-fixture")
                    .header("Cookie", "parity=yes")
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("cross-origin credential request must build"),
            )
            .unwrap_or_else(|error| {
                panic!("{backend_name}: cross-origin redirect failed: {error}")
            });
        assert_eq!(
            response.body(),
            b"authorization=false;cookie=false",
            "{backend_name}"
        );

        let redirect_loop = RedirectServer::start(RedirectScenario::Loop);
        match client
            .execute(
                Request::get(redirect_loop.url())
                    .redirect_limit(2)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("redirect loop request must build"),
            )
            .expect_err("redirect loop must fail")
        {
            ExecuteError::Failed(error) => {
                assert_eq!(error.kind(), ErrorKind::Redirect, "{backend_name}: {error}")
            }
            other => panic!("{backend_name}: expected redirect failure, got {other:?}"),
        }

        engine
            .shutdown()
            .unwrap_or_else(|error| panic!("{backend_name}: lab Engine failed to stop: {error}"));
    }
}

#[test]
fn malformed_status_headers_lengths_and_chunks_map_to_http() {
    for (backend_name, backend) in test_backends() {
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
                "transfer encoding with content length",
                &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 1\r\n\r\n1\r\nx\r\n0\r\n\r\n"[..],
            ),
            (
                "invalid chunk size",
                &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n"[..],
            ),
        ] {
            assert_transport(
                backend_name,
                backend,
                case,
                Script::Bytes(response),
                TransportStage::Http,
            );
        }
    }
}

#[test]
fn premature_eof_and_empty_response_map_to_receive() {
    for (backend_name, backend) in test_backends() {
        assert_transport(
            backend_name,
            backend,
            "short fixed-length body",
            Script::Bytes(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort"),
            TransportStage::Receive,
        );
        assert_transport(
            backend_name,
            backend,
            "empty response",
            Script::Bytes(b""),
            TransportStage::Receive,
        );
        assert_transport(
            backend_name,
            backend,
            "short chunk body",
            Script::Bytes(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhi"),
            TransportStage::Http,
        );
    }
}

#[test]
fn abortive_close_during_a_large_upload_maps_to_send() {
    const UPLOAD_BYTES: usize = 64 * 1024 * 1024;
    for (backend_name, backend) in test_backends() {
        let engine = test_engine(
            EngineConfig::spawned().with_max_request_body_bytes(UPLOAD_BYTES),
            backend,
        );
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
                    assert_eq!(
                        error.kind(),
                        ErrorKind::Transport,
                        "{backend_name}/trial {trial}: {error}"
                    );
                    assert_eq!(
                        error.transport_stage(),
                        Some(TransportStage::Send),
                        "{backend_name}/trial {trial}: {error}"
                    );
                }
                other => panic!(
                    "{backend_name}/trial {trial}: expected terminal send failure, got {other:?}"
                ),
            }
        }
        engine
            .shutdown()
            .unwrap_or_else(|error| panic!("{backend_name}: lab Engine failed to stop: {error}"));
    }
}

#[test]
fn response_body_header_byte_and_header_count_limits_match() {
    for (backend_name, backend) in test_backends() {
        for (case, config, response, expected) in [
            (
                "body bytes",
                EngineConfig::spawned().with_max_response_body_bytes(4),
                &b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"[..],
                LimitKind::ResponseBodyBytes,
            ),
            (
                "header bytes",
                EngineConfig::spawned().with_max_header_bytes(128),
                &b"HTTP/1.1 200 OK\r\nX-Lab: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..],
                LimitKind::ResponseHeaderBytes,
            ),
            (
                "header count",
                EngineConfig::spawned().with_max_header_count(4),
                &b"HTTP/1.1 200 OK\r\nX-1: a\r\nX-2: b\r\nX-3: c\r\nX-4: d\r\nX-5: e\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..],
                LimitKind::ResponseHeaderCount,
            ),
        ] {
            match execute(backend, config, Script::Bytes(response))
                .expect_err("oversize response must fail")
            {
                ExecuteError::Failed(error) => {
                    assert_eq!(
                        error.kind(),
                        ErrorKind::Limit,
                        "{backend_name}/{case}: {error}"
                    );
                    assert_eq!(
                        error.limit_kind(),
                        Some(expected),
                        "{backend_name}/{case}: {error}"
                    );
                }
                other => panic!(
                    "{backend_name}/{case}: expected terminal limit failure, got {other:?}"
                ),
            }
        }
    }
}

#[test]
fn explicit_no_verify_and_unknown_root_share_tls_outcomes() {
    for (backend_name, backend) in test_backends() {
        if !backend_has_tls(backend) {
            eprintln!("skipping {backend_name}: selected curl implementation has no TLS support");
            continue;
        }
        let identity = TlsIdentity::localhost();
        let server = TlsServer::start(&identity);
        let engine = test_engine(EngineConfig::spawned(), backend);
        let client = engine.client();

        let insecure = client
            .execute(
                Request::get(server.url())
                    .connect_timeout(Duration::from_secs(1))
                    .total_timeout(Duration::from_secs(2))
                    .tls_verification(TlsVerification::DangerouslyDisableCertificateVerification)
                    .build()
                    .expect("no-verify request must build"),
            )
            .unwrap_or_else(|error| panic!("{backend_name}: no-verify request failed: {error}"));
        assert_eq!(insecure.status(), 200, "{backend_name}");
        assert_eq!(insecure.body(), b"secure", "{backend_name}");

        match client
            .execute(
                Request::get(server.url())
                    .connect_timeout(Duration::from_secs(1))
                    .total_timeout(Duration::from_secs(2))
                    .tls_verification(TlsVerification::Verify)
                    .build()
                    .expect("unknown-root request must build"),
            )
            .expect_err("unknown-root request must fail")
        {
            ExecuteError::Failed(error) => {
                assert_eq!(
                    error.kind(),
                    ErrorKind::Transport,
                    "{backend_name}: {error}"
                );
                assert_eq!(
                    error.transport_stage(),
                    Some(TransportStage::Tls),
                    "{backend_name}: {error}"
                );
            }
            other => panic!("{backend_name}: expected TLS failure, got {other:?}"),
        }

        engine
            .shutdown()
            .unwrap_or_else(|error| panic!("{backend_name}: Engine failed to stop: {error}"));
    }
}

#[test]
fn total_and_inactivity_timeouts_close_the_stalled_socket() {
    for (backend_name, backend) in test_backends() {
        for timeout_kind in [TimeoutKind::Total, TimeoutKind::Inactivity] {
            let server = ScriptedServer::start(Script::StallAfterHead);
            let engine = test_engine(EngineConfig::spawned(), backend);
            let mut request = Request::get(server.url()).total_timeout(Duration::from_secs(2));
            request = match timeout_kind {
                TimeoutKind::Total => request.total_timeout(Duration::from_millis(150)),
                TimeoutKind::Inactivity => request.inactivity_timeout(Duration::from_millis(150)),
                _ => unreachable!("the parity fixture only selects portable request clocks"),
            };
            let result = engine
                .client()
                .execute(request.build().expect("timeout request must build"));
            match result.expect_err("stalled response must time out") {
                ExecuteError::Failed(error) => {
                    assert_eq!(
                        error.kind(),
                        ErrorKind::Timeout,
                        "{backend_name}/{timeout_kind:?}: {error}"
                    );
                    assert_eq!(
                        error.timeout_kind(),
                        Some(timeout_kind),
                        "{backend_name}/{timeout_kind:?}: {error}"
                    );
                }
                other => panic!(
                    "{backend_name}/{timeout_kind:?}: expected timeout failure, got {other:?}"
                ),
            }
            server.wait_for_request();
            server.wait_for_peer_close();
            engine.shutdown().unwrap_or_else(|error| {
                panic!("{backend_name}/{timeout_kind:?}: Engine failed to stop: {error}")
            });
        }
    }
}

#[test]
fn individual_cancel_closes_the_socket_and_commits_cancelled() {
    for (backend_name, backend) in test_backends() {
        let server = ScriptedServer::start(Script::StallAfterHead);
        let engine = test_engine(EngineConfig::spawned(), backend);
        let pending = engine
            .client()
            .submit(
                Request::get(server.url())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("cancel request must build"),
            )
            .unwrap_or_else(|error| panic!("{backend_name}: cancel request rejected: {error}"));
        server.wait_for_request();
        pending
            .handle()
            .cancel()
            .unwrap_or_else(|error| panic!("{backend_name}: cancel failed: {error}"));
        assert!(
            matches!(pending.wait(), Completion::Cancelled),
            "{backend_name}: cancellation must win"
        );
        server.wait_for_peer_close();
        engine
            .shutdown()
            .unwrap_or_else(|error| panic!("{backend_name}: Engine failed to stop: {error}"));
    }
}

#[test]
fn consuming_shutdown_closes_the_socket_and_releases_the_waiter() {
    for (backend_name, backend) in test_backends() {
        let server = ScriptedServer::start(Script::StallAfterHead);
        let engine = test_engine(EngineConfig::spawned(), backend);
        let pending = engine
            .client()
            .submit(
                Request::get(server.url())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("shutdown request must build"),
            )
            .unwrap_or_else(|error| panic!("{backend_name}: shutdown request rejected: {error}"));
        server.wait_for_request();
        engine
            .shutdown()
            .unwrap_or_else(|error| panic!("{backend_name}: Engine failed to stop: {error}"));
        assert!(
            matches!(pending.wait(), Completion::Cancelled),
            "{backend_name}: shutdown must cancel the accepted request"
        );
        server.wait_for_peer_close();
    }
}
