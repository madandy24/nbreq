use std::error::Error as StdError;
use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

/// Determines who progresses an [`Engine`](crate::Engine).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RunMode {
    /// NBReq owns the reactor thread.
    Spawned,
    /// The host progresses the engine explicitly with [`Engine::drive`](crate::Engine::drive).
    Manual,
}

/// Selects the HTTP implementation used by an [`Engine`](crate::Engine).
///
/// This enum selects HTTP only. It does not select DNS or TCP. With native support compiled, an
/// Engine using an explicit HTTP backend may still issue native DNS/TCP handles; without native
/// support, those operations fail [`ErrorKind::Unsupported`] before admission.
///
/// The variant remains present without the default `native` feature so portable configuration can
/// fail explicitly during Engine construction rather than changing type shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum HttpBackend {
    /// NBReq's Rust-native DNS, TCP, TLS, and HTTP implementation.
    Native,
}

/// Determines where owned callback jobs are executed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CallbackDispatch {
    /// Dispatch callbacks at safe points on the manual driving thread.
    Inline,
    /// Dispatch callbacks on an Engine-owned worker pool.
    Workers(NonZeroUsize),
}

/// Backend-neutral Engine configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineConfig {
    run_mode: RunMode,
    callback_dispatch: CallbackDispatch,
    max_inflight_requests: NonZeroUsize,
    command_queue_capacity: NonZeroUsize,
    callback_queue_capacity: NonZeroUsize,
    max_request_body_bytes: usize,
    max_response_body_bytes: usize,
    max_stream_queue_bytes_per_request: usize,
    max_stream_queued_bytes: usize,
    max_header_bytes: usize,
    max_header_count: usize,
    max_connections: NonZeroUsize,
    max_connections_per_origin: NonZeroUsize,
    max_idle_connections: usize,
    max_idle_connections_per_origin: usize,
    idle_connection_timeout: Duration,
    max_inflight_resolutions: NonZeroUsize,
    max_standalone_tcp_connections: NonZeroUsize,
    max_resolve_results: NonZeroUsize,
    max_tcp_queue_bytes_per_connection: usize,
}

const DEFAULT_BODY_LIMIT: usize = 16 * 1024 * 1024;
const DEFAULT_STREAM_QUEUE_LIMIT: usize = 256 * 1024;
const DEFAULT_STREAM_QUEUED_LIMIT: usize = 16 * 1024 * 1024;
const DEFAULT_HEADER_BYTES_LIMIT: usize = 64 * 1024;
const DEFAULT_HEADER_COUNT_LIMIT: usize = 256;
const DEFAULT_MAX_CONNECTIONS: usize = 32;
const DEFAULT_MAX_CONNECTIONS_PER_ORIGIN: usize = 8;
const DEFAULT_MAX_IDLE_CONNECTIONS: usize = 32;
const DEFAULT_MAX_IDLE_CONNECTIONS_PER_ORIGIN: usize = 4;
const DEFAULT_IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_INFLIGHT_RESOLUTIONS: usize = 256;
/// Native DNS transaction IDs reserved for HTTP-internal lookups.
pub(crate) const HTTP_DNS_TXID_RESERVE: usize = 1_024;
/// 16-bit DNS transaction-ID space.
pub(crate) const DNS_TRANSACTION_ID_SPACE: usize = u16::MAX as usize + 1;
/// Public resolutions may occupy at most this many native DNS transaction IDs.
pub(crate) const MAX_PUBLIC_DNS_TRANSACTIONS: usize =
    DNS_TRANSACTION_ID_SPACE - HTTP_DNS_TXID_RESERVE;
const DEFAULT_MAX_STANDALONE_TCP_CONNECTIONS: usize = 32;
const DEFAULT_MAX_RESOLVE_RESULTS: usize = 32;
const DEFAULT_TCP_QUEUE_LIMIT: usize = 256 * 1024;

impl EngineConfig {
    /// Returns the convenient default: an owned reactor and one callback worker.
    #[must_use]
    pub fn spawned() -> Self {
        Self {
            run_mode: RunMode::Spawned,
            callback_dispatch: CallbackDispatch::Workers(nonzero(1)),
            max_inflight_requests: nonzero(1_024),
            command_queue_capacity: nonzero(1_024),
            callback_queue_capacity: nonzero(1_024),
            max_request_body_bytes: DEFAULT_BODY_LIMIT,
            max_response_body_bytes: DEFAULT_BODY_LIMIT,
            max_stream_queue_bytes_per_request: DEFAULT_STREAM_QUEUE_LIMIT,
            max_stream_queued_bytes: DEFAULT_STREAM_QUEUED_LIMIT,
            max_header_bytes: DEFAULT_HEADER_BYTES_LIMIT,
            max_header_count: DEFAULT_HEADER_COUNT_LIMIT,
            max_connections: nonzero(DEFAULT_MAX_CONNECTIONS),
            max_connections_per_origin: nonzero(DEFAULT_MAX_CONNECTIONS_PER_ORIGIN),
            max_idle_connections: DEFAULT_MAX_IDLE_CONNECTIONS,
            max_idle_connections_per_origin: DEFAULT_MAX_IDLE_CONNECTIONS_PER_ORIGIN,
            idle_connection_timeout: DEFAULT_IDLE_CONNECTION_TIMEOUT,
            max_inflight_resolutions: nonzero(DEFAULT_MAX_INFLIGHT_RESOLUTIONS),
            max_standalone_tcp_connections: nonzero(DEFAULT_MAX_STANDALONE_TCP_CONNECTIONS),
            max_resolve_results: nonzero(DEFAULT_MAX_RESOLVE_RESULTS),
            max_tcp_queue_bytes_per_connection: DEFAULT_TCP_QUEUE_LIMIT,
        }
    }

    /// Returns a host-driven configuration with inline callback dispatch.
    #[must_use]
    pub fn manual() -> Self {
        Self {
            run_mode: RunMode::Manual,
            callback_dispatch: CallbackDispatch::Inline,
            max_inflight_requests: nonzero(1_024),
            command_queue_capacity: nonzero(1_024),
            callback_queue_capacity: nonzero(1_024),
            max_request_body_bytes: DEFAULT_BODY_LIMIT,
            max_response_body_bytes: DEFAULT_BODY_LIMIT,
            max_stream_queue_bytes_per_request: DEFAULT_STREAM_QUEUE_LIMIT,
            max_stream_queued_bytes: DEFAULT_STREAM_QUEUED_LIMIT,
            max_header_bytes: DEFAULT_HEADER_BYTES_LIMIT,
            max_header_count: DEFAULT_HEADER_COUNT_LIMIT,
            max_connections: nonzero(DEFAULT_MAX_CONNECTIONS),
            max_connections_per_origin: nonzero(DEFAULT_MAX_CONNECTIONS_PER_ORIGIN),
            max_idle_connections: DEFAULT_MAX_IDLE_CONNECTIONS,
            max_idle_connections_per_origin: DEFAULT_MAX_IDLE_CONNECTIONS_PER_ORIGIN,
            idle_connection_timeout: DEFAULT_IDLE_CONNECTION_TIMEOUT,
            max_inflight_resolutions: nonzero(DEFAULT_MAX_INFLIGHT_RESOLUTIONS),
            max_standalone_tcp_connections: nonzero(DEFAULT_MAX_STANDALONE_TCP_CONNECTIONS),
            max_resolve_results: nonzero(DEFAULT_MAX_RESOLVE_RESULTS),
            max_tcp_queue_bytes_per_connection: DEFAULT_TCP_QUEUE_LIMIT,
        }
    }

    /// Selects a spawned callback worker count.
    #[must_use]
    pub fn with_callback_workers(mut self, workers: NonZeroUsize) -> Self {
        self.callback_dispatch = CallbackDispatch::Workers(workers);
        self
    }

    /// Selects the maximum number of accepted requests that remain nonterminal or have a terminal
    /// callback still queued or running.
    #[must_use]
    pub fn with_max_inflight_requests(mut self, requests: NonZeroUsize) -> Self {
        self.max_inflight_requests = requests;
        self
    }

    /// Selects the command queue bound.
    #[must_use]
    pub fn with_command_queue_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.command_queue_capacity = capacity;
        self
    }

    /// Selects the callback event queue and callback-bearing request bound.
    #[must_use]
    pub fn with_callback_queue_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.callback_queue_capacity = capacity;
        self
    }

    /// Selects the maximum buffered request-body size. Zero permits only empty bodies.
    #[must_use]
    pub fn with_max_request_body_bytes(mut self, bytes: usize) -> Self {
        self.max_request_body_bytes = bytes;
        self
    }

    /// Selects the maximum buffered response-body size. Zero permits only empty bodies.
    #[must_use]
    pub fn with_max_response_body_bytes(mut self, bytes: usize) -> Self {
        self.max_response_body_bytes = bytes;
        self
    }

    /// Selects the maximum upload or unread-response flow-control window for one streaming
    /// request. Zero disables streaming admission.
    #[must_use]
    pub fn with_max_stream_queue_bytes_per_request(mut self, bytes: usize) -> Self {
        self.max_stream_queue_bytes_per_request = bytes;
        self
    }

    /// Selects the Engine-wide reserved streaming queue budget. Zero disables streaming
    /// admission.
    #[must_use]
    pub fn with_max_stream_queued_bytes(mut self, bytes: usize) -> Self {
        self.max_stream_queued_bytes = bytes;
        self
    }

    /// Selects the maximum cumulative bytes in one request or response head.
    #[must_use]
    pub fn with_max_header_bytes(mut self, bytes: usize) -> Self {
        self.max_header_bytes = bytes;
        self
    }

    /// Selects the maximum number of fields in one request or response head.
    #[must_use]
    pub fn with_max_header_count(mut self, count: usize) -> Self {
        self.max_header_count = count;
        self
    }

    /// Selects the maximum connecting, leased, and idle connections owned by the Engine.
    #[must_use]
    pub fn with_max_connections(mut self, connections: NonZeroUsize) -> Self {
        self.max_connections = connections;
        self
    }

    /// Selects the maximum connecting, leased, and idle connections for one origin.
    ///
    /// The global connection limit remains authoritative when it is smaller.
    #[must_use]
    pub fn with_max_connections_per_origin(mut self, connections: NonZeroUsize) -> Self {
        self.max_connections_per_origin = connections;
        self
    }

    /// Selects the maximum idle connections retained by the Engine. Zero disables idle reuse.
    #[must_use]
    pub fn with_max_idle_connections(mut self, connections: usize) -> Self {
        self.max_idle_connections = connections;
        self
    }

    /// Selects the maximum idle connections retained for one origin. Zero disables idle reuse.
    ///
    /// The global idle limit remains authoritative when it is smaller.
    #[must_use]
    pub fn with_max_idle_connections_per_origin(mut self, connections: usize) -> Self {
        self.max_idle_connections_per_origin = connections;
        self
    }

    /// Selects how long an unused persistent connection may remain pooled.
    ///
    /// A zero duration disables idle reuse.
    #[must_use]
    pub fn with_idle_connection_timeout(mut self, timeout: Duration) -> Self {
        self.idle_connection_timeout = timeout;
        self
    }

    /// Selects the public-resolver inflight budget. HTTP-internal DNS is reserved separately.
    ///
    /// The budget is capped so public work cannot consume the native DNS transaction-ID band
    /// reserved for HTTP-internal lookups.
    #[must_use]
    pub fn with_max_inflight_resolutions(mut self, resolutions: NonZeroUsize) -> Self {
        let capped = resolutions.get().min(MAX_PUBLIC_DNS_TRANSACTIONS);
        self.max_inflight_resolutions = NonZeroUsize::new(capped)
            .expect("HTTP DNS reservation leaves a public transaction cap");
        self
    }

    /// Selects the standalone TCP connect/live-connection budget.
    ///
    /// This is independent of the HTTP idle pool. The absolute Engine socket ceiling remains
    /// authoritative when it is smaller.
    #[must_use]
    pub fn with_max_standalone_tcp_connections(mut self, connections: NonZeroUsize) -> Self {
        self.max_standalone_tcp_connections = connections;
        self
    }

    /// Selects the Engine ceiling for addresses returned by one public resolution.
    ///
    /// Per-request [`ResolveRequest`](crate::ResolveRequest) caps may reduce this value, never
    /// exceed it.
    #[must_use]
    pub fn with_max_resolve_results(mut self, results: NonZeroUsize) -> Self {
        self.max_resolve_results = results;
        self
    }

    /// Selects the maximum send or unread-receive queue window for one standalone TCP connection.
    ///
    /// Zero disables standalone TCP admission once that facade is wired.
    #[must_use]
    pub fn with_max_tcp_queue_bytes_per_connection(mut self, bytes: usize) -> Self {
        self.max_tcp_queue_bytes_per_connection = bytes;
        self
    }

    /// Returns the configured run mode.
    #[must_use]
    pub fn run_mode(&self) -> RunMode {
        self.run_mode
    }

    /// Returns the configured callback dispatch mode.
    #[must_use]
    pub fn callback_dispatch(&self) -> CallbackDispatch {
        self.callback_dispatch
    }

    /// Returns the configured accepted/inflight request bound.
    #[must_use]
    pub fn max_inflight_requests(&self) -> NonZeroUsize {
        self.max_inflight_requests
    }

    /// Returns the configured command queue bound.
    #[must_use]
    pub fn command_queue_capacity(&self) -> NonZeroUsize {
        self.command_queue_capacity
    }

    /// Returns the configured callback event queue and callback-bearing request bound.
    #[must_use]
    pub fn callback_queue_capacity(&self) -> NonZeroUsize {
        self.callback_queue_capacity
    }

    /// Returns the maximum buffered request-body size.
    #[must_use]
    pub fn max_request_body_bytes(&self) -> usize {
        self.max_request_body_bytes
    }

    /// Returns the maximum buffered response-body size.
    #[must_use]
    pub fn max_response_body_bytes(&self) -> usize {
        self.max_response_body_bytes
    }

    /// Returns the maximum per-request streaming flow-control window.
    #[must_use]
    pub fn max_stream_queue_bytes_per_request(&self) -> usize {
        self.max_stream_queue_bytes_per_request
    }

    /// Returns the Engine-wide reserved streaming queue budget.
    #[must_use]
    pub fn max_stream_queued_bytes(&self) -> usize {
        self.max_stream_queued_bytes
    }

    /// Returns the maximum cumulative request/response header bytes.
    #[must_use]
    pub fn max_header_bytes(&self) -> usize {
        self.max_header_bytes
    }

    /// Returns the maximum request/response header field count.
    #[must_use]
    pub fn max_header_count(&self) -> usize {
        self.max_header_count
    }

    /// Returns the Engine-wide active connection limit.
    #[must_use]
    pub fn max_connections(&self) -> NonZeroUsize {
        self.max_connections
    }

    /// Returns the per-origin active connection limit.
    #[must_use]
    pub fn max_connections_per_origin(&self) -> NonZeroUsize {
        self.max_connections_per_origin
    }

    /// Returns the Engine-wide idle connection limit.
    #[must_use]
    pub fn max_idle_connections(&self) -> usize {
        self.max_idle_connections
    }

    /// Returns the per-origin idle connection limit.
    #[must_use]
    pub fn max_idle_connections_per_origin(&self) -> usize {
        self.max_idle_connections_per_origin
    }

    /// Returns the idle connection expiry duration.
    #[must_use]
    pub fn idle_connection_timeout(&self) -> Duration {
        self.idle_connection_timeout
    }

    /// Returns the public-resolver inflight budget.
    #[must_use]
    pub fn max_inflight_resolutions(&self) -> NonZeroUsize {
        self.max_inflight_resolutions
    }

    /// Returns the standalone TCP connect/live-connection budget.
    #[must_use]
    pub fn max_standalone_tcp_connections(&self) -> NonZeroUsize {
        self.max_standalone_tcp_connections
    }

    /// Returns the Engine ceiling for addresses returned by one public resolution.
    #[must_use]
    pub fn max_resolve_results(&self) -> NonZeroUsize {
        self.max_resolve_results
    }

    /// Returns the maximum send or unread-receive queue window for one standalone TCP connection.
    #[must_use]
    pub fn max_tcp_queue_bytes_per_connection(&self) -> usize {
        self.max_tcp_queue_bytes_per_connection
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self::spawned()
    }
}

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => panic!("NBReq internal non-zero constant was zero"),
    }
}

/// Result of one manual Engine driving pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DriveStatus {
    /// No work was ready before the supplied deadline.
    Idle,
    /// At least one internal state transition was made.
    Progress,
    /// The supplied deadline was reached.
    DeadlineReached,
}

/// An HTTP method.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Method {
    /// GET.
    Get,
    /// HEAD.
    Head,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
    /// An extension method.
    Other(String),
}

/// One owned HTTP header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    name: String,
    value: Vec<u8>,
}

impl Header {
    /// Creates an owned header. A containing [`RequestBuilder`] validates it when built.
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    /// Returns the header name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the header value bytes.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

/// Controls server-certificate and hostname verification for HTTPS requests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum TlsVerification {
    /// Verify the certificate chain and requested hostname.
    #[default]
    Verify,
    /// Disable both certificate-chain and hostname verification.
    ///
    /// This exists only for compatibility with deployments that cannot yet present a valid
    /// certificate. It is deliberately verbose and is never the default.
    DangerouslyDisableCertificateVerification,
}

/// Portable request policy and timeout options.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RequestOptions {
    /// Maximum time allowed to establish a connection.
    pub connect_timeout: Option<Duration>,
    /// Maximum time allowed without useful I/O progress across resolution, connection, and transfer.
    pub inactivity_timeout: Option<Duration>,
    /// Maximum total request duration.
    pub total_timeout: Option<Duration>,
    /// Maximum redirects followed under NBReq's conservative redirect policy.
    ///
    /// Zero returns the first redirect response without following it.
    pub redirect_limit: u8,
    /// HTTPS certificate and hostname verification policy.
    pub tls_verification: TlsVerification,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            connect_timeout: None,
            inactivity_timeout: None,
            total_timeout: None,
            redirect_limit: 5,
            tls_verification: TlsVerification::Verify,
        }
    }
}

/// An owned, backend-neutral HTTP request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    method: Method,
    url: String,
    headers: Vec<Header>,
    body: Vec<u8>,
    options: RequestOptions,
}

impl Request {
    /// Starts a request builder.
    #[must_use]
    pub fn builder(method: Method, url: impl Into<String>) -> RequestBuilder {
        RequestBuilder {
            request: Self {
                method,
                url: url.into(),
                headers: Vec::new(),
                body: Vec::new(),
                options: RequestOptions::default(),
            },
        }
    }

    /// Starts a GET request builder.
    #[must_use]
    pub fn get(url: impl Into<String>) -> RequestBuilder {
        Self::builder(Method::Get, url)
    }

    /// Starts a POST request builder.
    #[must_use]
    pub fn post(url: impl Into<String>) -> RequestBuilder {
        Self::builder(Method::Post, url)
    }

    /// Starts a HEAD request builder.
    #[must_use]
    pub fn head(url: impl Into<String>) -> RequestBuilder {
        Self::builder(Method::Head, url)
    }

    /// Starts a PUT request builder.
    #[must_use]
    pub fn put(url: impl Into<String>) -> RequestBuilder {
        Self::builder(Method::Put, url)
    }

    /// Starts a PATCH request builder.
    #[must_use]
    pub fn patch(url: impl Into<String>) -> RequestBuilder {
        Self::builder(Method::Patch, url)
    }

    /// Starts a DELETE request builder.
    #[must_use]
    pub fn delete(url: impl Into<String>) -> RequestBuilder {
        Self::builder(Method::Delete, url)
    }

    /// Returns the method.
    #[must_use]
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// Returns the URL text.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the request headers.
    #[must_use]
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    /// Returns the buffered request body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Returns the portable timeout options.
    #[must_use]
    pub fn options(&self) -> &RequestOptions {
        &self.options
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        http_origin(&self.url, ErrorKind::InvalidRequest)?;
        if matches!(&self.method, Method::Other(method) if !is_http_token(method)) {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "extension HTTP method is not a valid token",
            ));
        }
        for header in &self.headers {
            if !is_http_token(header.name()) || !is_valid_http_header_value(header.value()) {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "request contains an invalid HTTP header",
                ));
            }
        }
        Ok(())
    }

    #[cfg(any(feature = "native", test))]
    pub(crate) fn redirected(
        &self,
        url: String,
        method: Method,
        keep_body: bool,
        cross_origin: bool,
    ) -> Self {
        let headers = self
            .headers
            .iter()
            .filter(|header| {
                let name = header.name();
                let body_header = name.eq_ignore_ascii_case("content-length")
                    || name.eq_ignore_ascii_case("transfer-encoding");
                let origin_bound = name.eq_ignore_ascii_case("authorization")
                    || name.eq_ignore_ascii_case("proxy-authorization")
                    || name.eq_ignore_ascii_case("cookie")
                    || name.eq_ignore_ascii_case("host");
                (keep_body || !body_header) && (!cross_origin || !origin_bound)
            })
            .cloned()
            .collect();
        Self {
            method,
            url,
            headers,
            body: if keep_body {
                self.body.clone()
            } else {
                Vec::new()
            },
            options: self.options.clone(),
        }
    }
}

#[cfg(any(feature = "native", test))]
pub(crate) fn redirected_request(
    request: &Request,
    status: u16,
    redirect_hops: u8,
    target: impl FnOnce() -> Result<Option<String>, Error>,
) -> Result<Option<Request>, Error> {
    let (method, keep_body) = match status {
        301 | 302 => match request.method() {
            Method::Get | Method::Head => (request.method().clone(), true),
            _ => return Ok(None),
        },
        303 => match request.method() {
            Method::Head => (Method::Head, false),
            _ => (Method::Get, false),
        },
        307 | 308 => (request.method().clone(), true),
        _ => return Ok(None),
    };

    let limit = request.options().redirect_limit;
    if limit == 0 {
        return Ok(None);
    }
    if redirect_hops >= limit {
        return Err(Error::new(
            ErrorKind::Redirect,
            format!("redirect limit of {limit} hops was exceeded"),
        ));
    }
    let Some(target) = target()? else {
        return Ok(None);
    };
    let source_origin = http_origin(request.url(), ErrorKind::Redirect)?;
    let target_origin = http_origin(&target, ErrorKind::Redirect)?;
    if source_origin.scheme == "https" && target_origin.scheme == "http" {
        return Err(Error::new(
            ErrorKind::Redirect,
            "an HTTPS-to-HTTP redirect was blocked",
        ));
    }
    let cross_origin = source_origin != target_origin;
    Ok(Some(request.redirected(
        target,
        method,
        keep_body,
        cross_origin,
    )))
}

/// Builder for an owned [`Request`].
#[derive(Clone, Debug)]
pub struct RequestBuilder {
    request: Request,
}

impl RequestBuilder {
    /// Adds an owned header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.request.headers.push(Header::new(name, value));
        self
    }

    /// Sets the buffered body.
    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.request.body = body.into();
        self
    }

    /// Sets the portable timeout options.
    #[must_use]
    pub fn options(mut self, options: RequestOptions) -> Self {
        self.request.options = options;
        self
    }

    /// Sets the maximum connection-establishment duration.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.request.options.connect_timeout = Some(timeout);
        self
    }

    /// Sets the maximum duration without useful I/O progress.
    #[must_use]
    pub fn inactivity_timeout(mut self, timeout: Duration) -> Self {
        self.request.options.inactivity_timeout = Some(timeout);
        self
    }

    /// Sets the maximum total duration beginning at request acceptance.
    #[must_use]
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        self.request.options.total_timeout = Some(timeout);
        self
    }

    /// Sets the maximum number of redirects followed.
    #[must_use]
    pub fn redirect_limit(mut self, limit: u8) -> Self {
        self.request.options.redirect_limit = limit;
        self
    }

    /// Sets HTTPS certificate and hostname verification policy.
    #[must_use]
    pub fn tls_verification(mut self, verification: TlsVerification) -> Self {
        self.request.options.tls_verification = verification;
        self
    }

    /// Validates the backend-independent minimum and returns the request.
    pub fn build(self) -> Result<Request, Error> {
        self.request.validate()?;
        Ok(self.request)
    }
}

pub(crate) fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

pub(crate) fn is_valid_http_header_value(value: &[u8]) -> bool {
    !value
        .iter()
        .any(|byte| *byte == 0x7f || (*byte < 0x20 && *byte != b'\t'))
}

#[derive(Eq, PartialEq)]
pub(crate) struct HttpOrigin {
    pub(crate) scheme: String,
    pub(crate) host: String,
    pub(crate) port: u16,
}

pub(crate) fn http_origin(url: &str, error_kind: ErrorKind) -> Result<HttpOrigin, Error> {
    let Some((scheme, remainder)) = url.split_once("://") else {
        return Err(Error::new(error_kind, "HTTP URL has no scheme separator"));
    };
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => {
            return Err(Error::new(
                error_kind,
                "NBReq permits only HTTP and HTTPS URLs",
            ));
        }
    };
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return Err(Error::new(
            error_kind,
            "HTTP URL authority is empty or contains embedded credentials",
        ));
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some(closing) = bracketed.find(']') else {
            return Err(Error::new(error_kind, "HTTP URL has an invalid IPv6 host"));
        };
        let host = &bracketed[..closing];
        let suffix = &bracketed[closing + 1..];
        let port = if suffix.is_empty() {
            default_port
        } else if let Some(port) = suffix.strip_prefix(':') {
            parse_port(port, error_kind)?
        } else {
            return Err(Error::new(error_kind, "HTTP URL has an invalid authority"));
        };
        (host, port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') {
            return Err(Error::new(
                error_kind,
                "an IPv6 URL host must be enclosed in brackets",
            ));
        }
        (host, parse_port(port, error_kind)?)
    } else {
        (authority, default_port)
    };
    if host.is_empty() {
        return Err(Error::new(error_kind, "HTTP URL host is empty"));
    }
    if host
        .bytes()
        .any(|byte| byte <= 0x20 || byte == 0x7f || byte == b'\\')
    {
        return Err(Error::new(error_kind, "HTTP URL host is invalid"));
    }
    Ok(HttpOrigin {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
    })
}

fn parse_port(port: &str, error_kind: ErrorKind) -> Result<u16, Error> {
    port.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| Error::new(error_kind, "HTTP URL port is invalid"))
}

/// An owned, backend-neutral HTTP response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    status: u16,
    headers: Vec<Header>,
    body: Vec<u8>,
}

impl Response {
    /// Creates a buffered response value.
    #[must_use]
    pub fn new(status: u16, headers: Vec<Header>, body: Vec<u8>) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// Returns the HTTP status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Returns the response headers.
    ///
    /// The curl pilot buffers the final response head. Portable trailer representation is not yet
    /// defined; callers must not rely on trailers appearing here.
    #[must_use]
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    /// Returns the buffered response body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Opaque request identity, scoped to the Engine that issued it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId {
    pub(crate) engine: u64,
    pub(crate) sequence: u64,
}

/// The canonical terminal outcome of an accepted request.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Completion {
    /// A complete HTTP exchange, including HTTP error status codes.
    Completed(Response),
    /// A transport, protocol, policy, or timeout failure.
    Failed(Error),
    /// Explicit cancellation won the terminal-state race.
    Cancelled,
}

/// Stable high-level error classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The owning Engine no longer accepts work.
    EngineStopped,
    /// A request ID belongs to another Engine.
    WrongEngine,
    /// An operation is invalid for the Engine's run mode.
    WrongMode,
    /// A manual drive was attempted recursively or concurrently.
    ReentrantDrive,
    /// A blocking lifecycle operation was attempted from the Engine's callback/drive stack.
    ReentrantOperation,
    /// Request data failed backend-independent validation.
    InvalidRequest,
    /// A configured accepted/inflight, callback-event, or command capacity is exhausted.
    QueueFull,
    /// A configured request, response, or protocol resource limit was exceeded.
    Limit,
    /// No production backend implements the operation yet.
    BackendUnavailable,
    /// The selected backend cannot represent the requested portable feature.
    Unsupported,
    /// A connection, DNS, TLS, send, receive, or HTTP transport operation failed.
    Transport,
    /// A configured request timeout expired.
    Timeout,
    /// Redirect policy rejected a redirect or exhausted its configured hop limit.
    Redirect,
    /// Internal communication ended unexpectedly.
    Internal,
}

/// Portable timeout category attached to a timeout [`Error`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TimeoutKind {
    /// Connection establishment, including name resolution where the backend combines them.
    Connect,
    /// No useful request or response I/O progress occurred for the configured duration.
    Inactivity,
    /// The total operation deadline expired.
    ///
    /// Public DNS [`ResolveRequest::total_timeout`](crate::ResolveRequest::total_timeout) uses this
    /// category. The DNS facade already identifies the operation; timeout classification stays
    /// independent of [`DnsFailure`].
    Total,
    /// The backend reported a timeout but did not provide enough stage evidence to classify it.
    Unknown,
}

/// Portable transport stage attached to a transport [`Error`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TransportStage {
    /// Hostname or proxy-name resolution.
    Dns,
    /// TCP connection establishment.
    Connect,
    /// TLS configuration or handshake.
    Tls,
    /// Request transmission.
    Send,
    /// Response reception.
    Receive,
    /// HTTP response recognition or framing.
    Http,
}

/// Payload-free reason attached to a TLS-related transport [`Error`] when known.
///
/// This deliberately classifies failures without retaining hostnames, certificate contents, peer
/// alerts, or backend-native diagnostic text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TlsFailure {
    /// TLS configuration could not be constructed.
    Configuration,
    /// The certificate does not cover the requested hostname or IP address.
    CertificateHostnameMismatch,
    /// The certificate chain does not lead to a trusted issuer.
    CertificateUnknownIssuer,
    /// The certificate is expired.
    CertificateExpired,
    /// The certificate is not valid yet.
    CertificateNotYetValid,
    /// The certificate was revoked.
    CertificateRevoked,
    /// Certificate validation failed for another reason.
    CertificateInvalid,
    /// The peer sent a fatal TLS alert.
    PeerAlert,
    /// TLS framing, negotiation, cryptography, or protocol state failed.
    Protocol,
    /// Local encrypted-record input or output failed.
    Io,
    /// The implementation could not classify the TLS failure more precisely.
    Unknown,
}

/// Payload-free reason attached to a DNS-related transport [`Error`] when known.
///
/// NXDOMAIN and NoData are successful public-resolver exchanges and are not encoded here. Timeout
/// remains classified by [`TimeoutKind`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DnsFailure {
    /// The nameserver returned SERVFAIL.
    ServerFailure,
    /// The nameserver refused the query.
    Refused,
    /// The nameserver returned FORMERR or the reply could not be parsed.
    Malformed,
    /// The reply was truncated after the bounded TCP fallback.
    Truncated,
    /// No usable nameserver remained.
    NoNameserver,
    /// DNS protocol state failed after a well-formed exchange could not be completed.
    Protocol,
    /// Local resolver I/O failed.
    Io,
    /// The implementation could not classify the DNS failure more precisely.
    Unknown,
}

/// Portable resource category attached to a limit [`Error`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LimitKind {
    /// Buffered request body bytes.
    RequestBodyBytes,
    /// Request header bytes.
    RequestHeaderBytes,
    /// Request header field count.
    RequestHeaderCount,
    /// Buffered response body bytes.
    ResponseBodyBytes,
    /// Response header bytes.
    ResponseHeaderBytes,
    /// Response header field count.
    ResponseHeaderCount,
    /// Queued upload and unread response bytes reserved for streaming flow control.
    StreamingQueueBytes,
    /// Addresses requested from one public resolution.
    ResolveResults,
    /// Queued standalone TCP send or unread-receive bytes.
    TcpQueueBytes,
}

/// A backend-neutral NBReq error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    timeout_kind: Option<TimeoutKind>,
    transport_stage: Option<TransportStage>,
    tls_failure: Option<TlsFailure>,
    dns_failure: Option<DnsFailure>,
    limit_kind: Option<LimitKind>,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            timeout_kind: None,
            transport_stage: None,
            tls_failure: None,
            dns_failure: None,
            limit_kind: None,
        }
    }

    #[allow(dead_code)] // Used by real transports; the dependency-free scaffold has no timeout.
    pub(crate) fn timeout(kind: TimeoutKind, message: impl Into<String>) -> Self {
        let mut error = Self::new(ErrorKind::Timeout, message);
        error.timeout_kind = Some(kind);
        error
    }

    #[allow(dead_code)] // Used by real transports; the dependency-free scaffold has no transport.
    pub(crate) fn transport(stage: TransportStage, message: impl Into<String>) -> Self {
        let mut error = Self::new(ErrorKind::Transport, message);
        error.transport_stage = Some(stage);
        error
    }

    #[allow(dead_code)] // Used by the native TLS backend; minimal builds retain the public detail.
    pub(crate) fn tls(
        stage: TransportStage,
        failure: TlsFailure,
        message: impl Into<String>,
    ) -> Self {
        let mut error = Self::transport(stage, message);
        error.tls_failure = Some(failure);
        error
    }

    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) fn dns(failure: DnsFailure, message: impl Into<String>) -> Self {
        let mut error = Self::transport(TransportStage::Dns, message);
        error.dns_failure = Some(failure);
        error
    }

    pub(crate) fn limit(kind: LimitKind, message: impl Into<String>) -> Self {
        let mut error = Self::new(ErrorKind::Limit, message);
        error.limit_kind = Some(kind);
        error
    }

    /// Returns the stable high-level classification.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Returns a payload-free diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the portable timeout category when this is a timeout error.
    #[must_use]
    pub fn timeout_kind(&self) -> Option<TimeoutKind> {
        self.timeout_kind
    }

    /// Returns the portable transport stage when one is known.
    #[must_use]
    pub fn transport_stage(&self) -> Option<TransportStage> {
        self.transport_stage
    }

    /// Returns a payload-free TLS failure reason when one is known.
    #[must_use]
    pub fn tls_failure(&self) -> Option<TlsFailure> {
        self.tls_failure
    }

    /// Returns a payload-free DNS failure reason when one is known.
    #[must_use]
    pub fn dns_failure(&self) -> Option<DnsFailure> {
        self.dns_failure
    }

    /// Returns the violated portable resource limit when this is a limit error.
    #[must_use]
    pub fn limit_kind(&self) -> Option<LimitKind> {
        self.limit_kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for Error {}

/// Error returned by the blocking convenience API.
#[derive(Debug)]
#[non_exhaustive]
pub enum ExecuteError {
    /// The operation was rejected before acceptance.
    Submission(Error),
    /// An accepted operation reached a failed terminal state.
    Failed(Error),
    /// An accepted operation was cancelled.
    Cancelled,
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Submission(error) => write!(formatter, "operation was not accepted: {error}"),
            Self::Failed(error) => write!(formatter, "operation failed: {error}"),
            Self::Cancelled => formatter.write_str("operation was cancelled"),
        }
    }
}

impl StdError for ExecuteError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Submission(error) | Self::Failed(error) => Some(error),
            Self::Cancelled => None,
        }
    }
}

/// Failure while irreversibly stopping an Engine or callback domain.
#[derive(Debug)]
pub struct ShutdownError {
    source: Error,
}

impl ShutdownError {
    pub(crate) fn new(source: Error) -> Self {
        Self { source }
    }

    /// Returns the underlying stable NBReq error.
    #[must_use]
    pub fn error(&self) -> &Error {
        &self.source
    }
}

impl fmt::Display for ShutdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Engine shutdown failed: {}", self.source)
    }
}

impl StdError for ShutdownError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_resolution_budget_cannot_consume_http_dns_txid_reserve() {
        let huge = NonZeroUsize::new(usize::MAX).expect("usize::MAX is non-zero");
        let config = EngineConfig::spawned().with_max_inflight_resolutions(huge);
        assert_eq!(
            config.max_inflight_resolutions().get(),
            MAX_PUBLIC_DNS_TRANSACTIONS
        );
        assert_eq!(
            config.max_inflight_resolutions().get() + HTTP_DNS_TXID_RESERVE,
            DNS_TRANSACTION_ID_SPACE
        );
    }

    #[test]
    fn request_builder_rejects_non_http_urls_invalid_tokens_and_header_injection() {
        for builder in [
            Request::get(""),
            Request::get("ftp://example.test/"),
            Request::get("http://user@example.test/"),
            Request::get("http://bad host/"),
            Request::get("http://example.test:0/"),
            Request::builder(
                Method::Other("NOT VALID".to_owned()),
                "http://example.test/",
            ),
            Request::get("http://example.test/").header("bad:name", "value"),
            Request::get("http://example.test/").header("good-name", b"bad\r\nvalue".to_vec()),
            Request::get("http://example.test/").header("good-name", b"bad\0value".to_vec()),
        ] {
            let error = builder
                .build()
                .expect_err("invalid request must fail during construction");
            assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        }
    }

    #[test]
    fn request_builder_convenience_methods_set_portable_policy() {
        let request = Request::builder(Method::Other("PURGE".to_owned()), "https://[::1]:8443/")
            .connect_timeout(Duration::from_millis(100))
            .inactivity_timeout(Duration::from_millis(200))
            .total_timeout(Duration::from_millis(300))
            .redirect_limit(2)
            .tls_verification(TlsVerification::DangerouslyDisableCertificateVerification)
            .build()
            .expect("valid portable request must build");

        assert_eq!(
            request.options.connect_timeout,
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            request.options.inactivity_timeout,
            Some(Duration::from_millis(200))
        );
        assert_eq!(
            request.options.total_timeout,
            Some(Duration::from_millis(300))
        );
        assert_eq!(request.options.redirect_limit, 2);
        assert_eq!(
            request.options.tls_verification,
            TlsVerification::DangerouslyDisableCertificateVerification
        );
    }

    #[test]
    fn redirect_policy_is_lazy_and_preserves_head_on_303() {
        let post = Request::post("https://example.test/start")
            .body(b"payload".to_vec())
            .build()
            .expect("redirect source must build");
        let not_followed = redirected_request(&post, 302, 0, || {
            panic!("a non-followed redirect must not inspect Location")
        })
        .expect("302 POST must remain a response");
        assert!(not_followed.is_none());

        let disabled = Request::get("https://example.test/start")
            .redirect_limit(0)
            .build()
            .expect("disabled redirect source must build");
        let not_followed = redirected_request(&disabled, 302, 0, || {
            panic!("disabled redirects must not inspect Location")
        })
        .expect("disabled redirect must remain a response");
        assert!(not_followed.is_none());

        let head = Request::head("https://example.test/start")
            .build()
            .expect("HEAD redirect source must build");
        let redirected = redirected_request(&head, 303, 0, || {
            Ok(Some("https://example.test/final".to_owned()))
        })
        .expect("303 HEAD policy must succeed")
        .expect("303 HEAD must follow");
        assert_eq!(redirected.method(), &Method::Head);
        assert!(redirected.body().is_empty());

        let downgrade = redirected_request(&head, 307, 0, || {
            Ok(Some("http://example.test/final".to_owned()))
        })
        .expect_err("HTTPS downgrade must be blocked");
        assert_eq!(downgrade.kind(), ErrorKind::Redirect);
    }
}
