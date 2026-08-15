use std::error::Error as StdError;
use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

/// Determines who progresses an [`Engine`](crate::Engine).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunMode {
    /// NBReq owns the reactor thread.
    Spawned,
    /// The host progresses the engine explicitly with [`Engine::drive`](crate::Engine::drive).
    Manual,
}

/// Determines where owned callback jobs are executed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    command_queue_capacity: NonZeroUsize,
    callback_queue_capacity: NonZeroUsize,
}

impl EngineConfig {
    /// Returns the convenient default: an owned reactor and one callback worker.
    #[must_use]
    pub fn spawned() -> Self {
        Self {
            run_mode: RunMode::Spawned,
            callback_dispatch: CallbackDispatch::Workers(nonzero(1)),
            command_queue_capacity: nonzero(1_024),
            callback_queue_capacity: nonzero(1_024),
        }
    }

    /// Returns a host-driven configuration with inline callback dispatch.
    #[must_use]
    pub fn manual() -> Self {
        Self {
            run_mode: RunMode::Manual,
            callback_dispatch: CallbackDispatch::Inline,
            command_queue_capacity: nonzero(1_024),
            callback_queue_capacity: nonzero(1_024),
        }
    }

    /// Selects a spawned callback worker count.
    #[must_use]
    pub fn with_callback_workers(mut self, workers: NonZeroUsize) -> Self {
        self.callback_dispatch = CallbackDispatch::Workers(workers);
        self
    }

    /// Selects the command queue bound.
    #[must_use]
    pub fn with_command_queue_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.command_queue_capacity = capacity;
        self
    }

    /// Selects the callback event queue bound.
    #[must_use]
    pub fn with_callback_queue_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.callback_queue_capacity = capacity;
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

    /// Returns the configured command queue bound.
    #[must_use]
    pub fn command_queue_capacity(&self) -> NonZeroUsize {
        self.command_queue_capacity
    }

    /// Returns the configured callback event queue bound.
    #[must_use]
    pub fn callback_queue_capacity(&self) -> NonZeroUsize {
        self.callback_queue_capacity
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
    /// Creates an owned header. Strict protocol validation is added with the HTTP layer.
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

/// Portable request timeout options.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RequestOptions {
    /// Maximum time allowed to establish a connection.
    pub connect_timeout: Option<Duration>,
    /// Maximum time allowed without useful I/O progress.
    pub inactivity_timeout: Option<Duration>,
    /// Maximum total request duration.
    pub total_timeout: Option<Duration>,
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

    /// Validates the backend-independent minimum and returns the request.
    pub fn build(self) -> Result<Request, Error> {
        if self.request.url.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "request URL is empty",
            ));
        }
        if matches!(&self.request.method, Method::Other(method) if method.trim().is_empty()) {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "extension HTTP method is empty",
            ));
        }
        Ok(self.request)
    }
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
    /// The configured bounded admission or command capacity is exhausted.
    QueueFull,
    /// No production backend implements the operation yet.
    BackendUnavailable,
    /// The selected backend cannot represent the requested portable feature.
    Unsupported,
    /// Internal communication ended unexpectedly.
    Internal,
}

/// A backend-neutral NBReq error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
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
    /// The request was rejected before acceptance.
    Submission(Error),
    /// An accepted request reached a failed terminal state.
    Failed(Error),
    /// An accepted request was cancelled.
    Cancelled,
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Submission(error) => write!(formatter, "request was not accepted: {error}"),
            Self::Failed(error) => write!(formatter, "request failed: {error}"),
            Self::Cancelled => formatter.write_str("request was cancelled"),
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
