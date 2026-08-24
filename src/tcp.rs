//! Public cleartext TCP connector contract.
//!
//! [`TcpConnector`] is an Engine-issued capability ticket. In this F0 skeleton, connect operations
//! fail [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported) before identity allocation,
//! admission, callback reservation, or command queuing. Live connections are never constructed.

use std::cell::Cell;
use std::error::Error as StdError;
use std::fmt;
use std::marker::PhantomData;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::dns::normalize_dns_name;
use crate::registry::Shared;
use crate::{Error, ErrorKind, ExecuteError, LimitKind, RequestId};

/// Destination of one cleartext TCP connect request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TcpConnectTarget {
    /// Resolve this exact hostname, then connect.
    Hostname {
        /// Normalized lookup identity: lowercase ASCII without a terminal dot.
        name: String,
        /// Destination port. Port zero is rejected at build time.
        port: u16,
    },
    /// Connect to this literal address without public DNS.
    Literal(SocketAddr),
}

/// An owned cleartext TCP connect request.
///
/// Connect timeout begins at admission and covers queued DNS plus TCP connection establishment; it
/// ends when the live connection is produced. Optional connected-phase read and write inactivity
/// policies then apply separately. Blocking connected operations reject manual mode rather than
/// driving it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcpConnectRequest {
    target: TcpConnectTarget,
    connect_timeout: Option<Duration>,
    read_inactivity_timeout: Option<Duration>,
    write_inactivity_timeout: Option<Duration>,
    send_queue_bytes: Option<usize>,
    receive_queue_bytes: Option<usize>,
}

impl TcpConnectRequest {
    /// Starts a connect builder for an exact ASCII or punycode hostname and port.
    #[must_use]
    pub fn hostname(name: impl Into<String>, port: u16) -> TcpConnectRequestBuilder {
        TcpConnectRequestBuilder {
            target: HostnameOrLiteral::Hostname {
                name: name.into(),
                port,
            },
            connect_timeout: None,
            read_inactivity_timeout: None,
            write_inactivity_timeout: None,
            send_queue_bytes: None,
            receive_queue_bytes: None,
        }
    }

    /// Starts a connect builder for a literal socket address. DNS is skipped.
    #[must_use]
    pub fn literal(addr: SocketAddr) -> TcpConnectRequestBuilder {
        TcpConnectRequestBuilder {
            target: HostnameOrLiteral::Literal(addr),
            connect_timeout: None,
            read_inactivity_timeout: None,
            write_inactivity_timeout: None,
            send_queue_bytes: None,
            receive_queue_bytes: None,
        }
    }

    /// Returns the connect destination.
    #[must_use]
    pub fn target(&self) -> &TcpConnectTarget {
        &self.target
    }

    /// Returns the connect timeout covering capacity waiting, DNS when required, and TCP
    /// connection establishment. The clock ends when the live connection is produced.
    #[must_use]
    pub fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    /// Returns the connected-phase read inactivity timeout when one was selected.
    ///
    /// Read inactivity pauses while the consumer has not provided destination capacity.
    #[must_use]
    pub fn read_inactivity_timeout(&self) -> Option<Duration> {
        self.read_inactivity_timeout
    }

    /// Returns the connected-phase write inactivity timeout when one was selected.
    ///
    /// Write inactivity runs only while accepted output is waiting for socket progress.
    #[must_use]
    pub fn write_inactivity_timeout(&self) -> Option<Duration> {
        self.write_inactivity_timeout
    }

    /// Returns the per-connection send-queue window when one was selected.
    #[must_use]
    pub fn send_queue_bytes(&self) -> Option<usize> {
        self.send_queue_bytes
    }

    /// Returns the per-connection unread-receive window when one was selected.
    #[must_use]
    pub fn receive_queue_bytes(&self) -> Option<usize> {
        self.receive_queue_bytes
    }
}

#[derive(Clone, Debug)]
enum HostnameOrLiteral {
    Hostname { name: String, port: u16 },
    Literal(SocketAddr),
}

/// Builder for an owned [`TcpConnectRequest`].
#[derive(Clone, Debug)]
pub struct TcpConnectRequestBuilder {
    target: HostnameOrLiteral,
    connect_timeout: Option<Duration>,
    read_inactivity_timeout: Option<Duration>,
    write_inactivity_timeout: Option<Duration>,
    send_queue_bytes: Option<usize>,
    receive_queue_bytes: Option<usize>,
}

impl TcpConnectRequestBuilder {
    /// Sets the connect timeout covering capacity waiting, DNS when required, and connection
    /// establishment.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets the connected-phase read inactivity timeout.
    ///
    /// The clock pauses under consumer backpressure, when no destination capacity is available.
    /// Blocking `read` rejects manual mode rather than driving the Engine.
    #[must_use]
    pub fn read_inactivity_timeout(mut self, timeout: Duration) -> Self {
        self.read_inactivity_timeout = Some(timeout);
        self
    }

    /// Sets the connected-phase write inactivity timeout.
    ///
    /// The clock runs only while accepted output is waiting for socket progress. Blocking `send`
    /// and `finish` reject manual mode rather than driving the Engine.
    #[must_use]
    pub fn write_inactivity_timeout(mut self, timeout: Duration) -> Self {
        self.write_inactivity_timeout = Some(timeout);
        self
    }

    /// Selects a per-connection send-queue window at or below the Engine ceiling.
    ///
    /// Zero is rejected at build time. A value above the Engine ceiling is rejected at start,
    /// submit, or execute without allocating an ID or permit.
    #[must_use]
    pub fn send_queue_bytes(mut self, bytes: usize) -> Self {
        self.send_queue_bytes = Some(bytes);
        self
    }

    /// Selects a per-connection unread-receive window at or below the Engine ceiling.
    #[must_use]
    pub fn receive_queue_bytes(mut self, bytes: usize) -> Self {
        self.receive_queue_bytes = Some(bytes);
        self
    }

    /// Validates the destination and queue windows, then returns the request.
    pub fn build(self) -> Result<TcpConnectRequest, Error> {
        if self.send_queue_bytes == Some(0) || self.receive_queue_bytes == Some(0) {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "a TCP queue window must be greater than zero",
            ));
        }
        let target = match self.target {
            HostnameOrLiteral::Hostname { name, port } => {
                if port == 0 {
                    return Err(Error::new(
                        ErrorKind::InvalidRequest,
                        "TCP destination port zero is not allowed",
                    ));
                }
                TcpConnectTarget::Hostname {
                    name: normalize_dns_name(&name)?.identity,
                    port,
                }
            }
            HostnameOrLiteral::Literal(addr) => {
                if addr.port() == 0 {
                    return Err(Error::new(
                        ErrorKind::InvalidRequest,
                        "TCP destination port zero is not allowed",
                    ));
                }
                if is_unspecified_destination(addr.ip()) {
                    return Err(Error::new(
                        ErrorKind::InvalidRequest,
                        "TCP literal destinations cannot be unspecified addresses",
                    ));
                }
                TcpConnectTarget::Literal(addr)
            }
        };
        Ok(TcpConnectRequest {
            target,
            connect_timeout: self.connect_timeout,
            read_inactivity_timeout: self.read_inactivity_timeout,
            write_inactivity_timeout: self.write_inactivity_timeout,
            send_queue_bytes: self.send_queue_bytes,
            receive_queue_bytes: self.receive_queue_bytes,
        })
    }
}

fn is_unspecified_destination(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_unspecified(),
        IpAddr::V6(addr) => {
            addr.is_unspecified()
                || Ipv6Addr::to_ipv4_mapped(&addr).is_some_and(|mapped| mapped.is_unspecified())
        }
    }
}

/// Cheap cloneable cleartext TCP handle issued by an [`Engine`](crate::Engine).
///
/// TcpConnector has no public constructor. It does not own or extend Engine lifetime. Detached
/// handles reject new work with [`ErrorKind::EngineStopped`].
///
/// ```compile_fail
/// let _ = nbreq::TcpConnector::new();
/// ```
#[derive(Clone, Debug)]
pub struct TcpConnector {
    shared: Arc<Shared>,
    max_tcp_queue_bytes_per_connection: usize,
}

impl TcpConnector {
    pub(crate) fn new(shared: Arc<Shared>, max_tcp_queue_bytes_per_connection: usize) -> Self {
        Self {
            shared,
            max_tcp_queue_bytes_per_connection,
        }
    }

    /// Starts a callback-oriented connect.
    ///
    /// Rejected starts drop the callback without invoking it.
    pub fn start<F>(
        &self,
        request: TcpConnectRequest,
        callback: F,
    ) -> Result<TcpConnectHandle, Error>
    where
        F: FnOnce(TcpConnectCompletion) + Send + 'static,
    {
        self.reject_connect(request)?;
        drop(callback);
        Err(self.unavailable())
    }

    /// Submits a connect and returns its direct terminal-state waiter.
    pub fn submit(&self, request: TcpConnectRequest) -> Result<PendingTcpConnect, Error> {
        self.reject_connect(request)?;
        Err(self.unavailable())
    }

    /// Submits a connect and blocks on its direct terminal-state waiter.
    pub fn execute(&self, request: TcpConnectRequest) -> Result<TcpConnection, ExecuteError> {
        match self.submit(request) {
            Ok(pending) => match pending.wait() {
                TcpConnectCompletion::Completed(connection) => Ok(connection),
                TcpConnectCompletion::Failed(error) => Err(ExecuteError::Failed(error)),
                TcpConnectCompletion::Cancelled => Err(ExecuteError::Cancelled),
            },
            Err(error) => Err(ExecuteError::Submission(error)),
        }
    }

    /// Cancels an Engine-scoped connect ID.
    ///
    /// Standalone TCP is not wired in F0, so this rejects before touching the HTTP registry.
    pub fn cancel(&self, _request_id: RequestId) -> Result<(), Error> {
        Err(self.stopped_or_unavailable())
    }

    fn reject_connect(&self, request: TcpConnectRequest) -> Result<(), Error> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(stopped_error());
        }
        for window in [request.send_queue_bytes, request.receive_queue_bytes]
            .into_iter()
            .flatten()
        {
            if window > self.max_tcp_queue_bytes_per_connection {
                return Err(Error::limit(
                    LimitKind::TcpQueueBytes,
                    format!(
                        "TCP queue window exceeds the Engine ceiling of {} bytes",
                        self.max_tcp_queue_bytes_per_connection
                    ),
                ));
            }
        }
        Ok(())
    }

    fn unavailable(&self) -> Error {
        Error::new(
            ErrorKind::Unsupported,
            "standalone TCP connections are not available yet",
        )
    }

    fn stopped_or_unavailable(&self) -> Error {
        if self.shared.stopped.load(Ordering::Acquire) {
            stopped_error()
        } else {
            self.unavailable()
        }
    }
}

/// Engine-bound control handle for one accepted connect operation.
#[derive(Clone, Debug)]
pub struct TcpConnectHandle {
    connector: TcpConnector,
    id: RequestId,
}

impl TcpConnectHandle {
    #[allow(dead_code)] // Constructed when TcpConnector wiring begins.
    pub(crate) fn new(connector: TcpConnector, id: RequestId) -> Self {
        Self { connector, id }
    }

    /// Returns the opaque Engine-scoped identity.
    #[must_use]
    pub fn id(&self) -> RequestId {
        self.id
    }

    /// Requests cancellation of the in-flight connect.
    pub fn cancel(&self) -> Result<(), Error> {
        self.connector.cancel(self.id)
    }
}

/// Canonical terminal outcome of an accepted connect.
///
/// The completed value is unique and not cloneable.
#[derive(Debug)]
#[non_exhaustive]
pub enum TcpConnectCompletion {
    /// A unique live cleartext connection.
    Completed(TcpConnection),
    /// A transport, protocol, timeout, or policy failure.
    Failed(Error),
    /// Explicit cancellation won the terminal-state race.
    Cancelled,
}

/// Result of a waiter-local connect timeout.
#[derive(Debug)]
#[non_exhaustive]
pub enum TcpConnectWaitOutcome {
    /// The connect reached its canonical terminal outcome.
    Completed(TcpConnectCompletion),
    /// The local wait expired; the connect and its cancellation handle remain live.
    TimedOut(PendingTcpConnect),
}

/// Accepted connect plus a direct terminal-state waiter.
#[derive(Debug)]
pub struct PendingTcpConnect {
    handle: TcpConnectHandle,
}

impl PendingTcpConnect {
    #[allow(dead_code)] // Constructed when TcpConnector wiring begins.
    pub(crate) fn new(handle: TcpConnectHandle) -> Self {
        Self { handle }
    }

    /// Returns a clone of the independent cancellation handle.
    #[must_use]
    pub fn handle(&self) -> TcpConnectHandle {
        self.handle.clone()
    }

    /// Returns whether canonical terminal state has been committed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        false
    }

    pub(crate) fn try_completion(&self) -> Option<TcpConnectCompletion> {
        None
    }

    pub(crate) fn issued_engine_id(&self) -> u64 {
        self.handle.id.engine
    }

    /// Waits for and returns the canonical terminal outcome.
    #[must_use]
    pub fn wait(self) -> TcpConnectCompletion {
        let _ = self;
        TcpConnectCompletion::Failed(Error::new(
            ErrorKind::Unsupported,
            "standalone TCP connections are not available yet",
        ))
    }

    /// Waits locally without changing connect state or cancelling on timeout.
    #[must_use]
    pub fn wait_for(self, _duration: Duration) -> TcpConnectWaitOutcome {
        TcpConnectWaitOutcome::TimedOut(self)
    }
}

/// Cloneable cancellation handle for one live TCP connection.
#[derive(Clone, Debug)]
pub struct TcpConnectionHandle {
    connector: TcpConnector,
    id: RequestId,
}

impl TcpConnectionHandle {
    #[allow(dead_code)] // Constructed when TcpConnector wiring begins.
    pub(crate) fn new(connector: TcpConnector, id: RequestId) -> Self {
        Self { connector, id }
    }

    /// Returns the opaque Engine-scoped identity.
    #[must_use]
    pub fn id(&self) -> RequestId {
        self.id
    }

    /// Requests abortive cancellation of the live connection.
    pub fn cancel(&self) -> Result<(), Error> {
        self.connector.cancel(self.id)
    }
}

/// Unique unsplit cleartext TCP connection.
///
/// The connection is `Send` and deliberately neither `Clone` nor `Sync`. There is no `into_std`,
/// raw descriptor, escaped socket, or TLS mode.
///
/// Dropping the unsplit connection before its writer has finished and its reader has observed EOF
/// aborts it. Dropping it after both directions are terminal is harmless.
///
/// ```compile_fail
/// fn require_clone<T: Clone>() {}
/// require_clone::<nbreq::TcpConnection>();
/// ```
///
/// ```compile_fail
/// fn require_sync<T: Sync>() {}
/// require_sync::<nbreq::TcpConnection>();
/// ```
pub struct TcpConnection {
    _not_sync: PhantomData<Cell<()>>,
}

impl fmt::Debug for TcpConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TcpConnection")
            .finish_non_exhaustive()
    }
}

impl TcpConnection {
    /// Returns an independent cancellation-only handle for this connection.
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        unwired_live_tcp()
    }

    /// Returns the local socket address once wired.
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        unwired_live_tcp()
    }

    /// Returns the peer socket address once wired.
    pub fn peer_addr(&self) -> Result<SocketAddr, Error> {
        unwired_live_tcp()
    }

    /// Consumes the unsplit connection into unique reader and writer halves.
    #[must_use]
    pub fn split(self) -> (TcpReader, TcpWriter) {
        unwired_live_tcp()
    }

    /// Attempts to read available bytes without blocking.
    pub fn try_read(&mut self, _destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        unwired_live_tcp()
    }

    /// Reads bytes on a spawned Engine. Manual Engines return `WrongMode` rather than driving.
    pub fn read(&mut self, _destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        unwired_live_tcp()
    }

    /// Attempts to queue owned bytes without blocking. Refused input is returned unchanged.
    pub fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        Err(TcpSendError::unwired(bytes))
    }

    /// Queues owned bytes on a spawned Engine, returning any unaccepted suffix.
    ///
    /// Manual Engines return [`TcpSendErrorKind::WrongMode`] rather than driving.
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        Err(TcpSendError::unwired(bytes))
    }

    /// Drains accepted output and then half-closes the write side.
    ///
    /// This takes `&mut self` so the unsplit reader remains usable after write-half-close. The
    /// split writer uses consuming [`TcpWriter::finish`]. Manual Engines reject blocking finish
    /// with [`TcpFinishError::WrongMode`] rather than driving internally.
    pub fn finish(&mut self) -> Result<(), TcpFinishError> {
        Err(TcpFinishError::unwired())
    }
}

/// Unique TCP receiving half.
///
/// Dropping the reader before observed EOF cancels the connection, including after writer finish.
/// Dropping it after EOF is harmless.
///
/// ```compile_fail
/// fn require_clone<T: Clone>() {}
/// require_clone::<nbreq::TcpReader>();
/// ```
///
/// ```compile_fail
/// fn require_sync<T: Sync>() {}
/// require_sync::<nbreq::TcpReader>();
/// ```
pub struct TcpReader {
    _not_sync: PhantomData<Cell<()>>,
}

impl fmt::Debug for TcpReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TcpReader").finish_non_exhaustive()
    }
}

impl TcpReader {
    /// Returns an independent cancellation-only handle for this connection.
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        unwired_live_tcp()
    }

    /// Attempts to read available bytes without blocking.
    pub fn try_read(&mut self, _destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        unwired_live_tcp()
    }

    /// Reads bytes on a spawned Engine. Manual Engines return `WrongMode` rather than driving.
    pub fn read(&mut self, _destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        unwired_live_tcp()
    }
}

/// Unique TCP sending half.
///
/// Dropping the writer before [`Self::finish`] aborts the connection. `finish(self)` drains
/// accepted output and then write-half-closes.
///
/// ```compile_fail
/// fn require_clone<T: Clone>() {}
/// require_clone::<nbreq::TcpWriter>();
/// ```
///
/// ```compile_fail
/// fn require_sync<T: Sync>() {}
/// require_sync::<nbreq::TcpWriter>();
/// ```
pub struct TcpWriter {
    _not_sync: PhantomData<Cell<()>>,
}

impl fmt::Debug for TcpWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TcpWriter").finish_non_exhaustive()
    }
}

impl TcpWriter {
    /// Returns an independent cancellation-only handle for this connection.
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        unwired_live_tcp()
    }

    /// Attempts to queue owned bytes without blocking. Refused input is returned unchanged.
    pub fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        Err(TcpSendError::unwired(bytes))
    }

    /// Queues owned bytes on a spawned Engine, returning any unaccepted suffix.
    ///
    /// Manual Engines return [`TcpSendErrorKind::WrongMode`] rather than driving.
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        Err(TcpSendError::unwired(bytes))
    }

    /// Drains accepted output and then half-closes the write side.
    ///
    /// Manual Engines reject blocking finish with [`TcpFinishError::WrongMode`] rather than
    /// driving internally.
    pub fn finish(self) -> Result<(), TcpFinishError> {
        let _ = self;
        Err(TcpFinishError::unwired())
    }
}

fn unwired_live_tcp() -> ! {
    // F0-only: these types are unconstructible until F2 wires TcpConnector. Replace this body
    // as soon as live connections become obtainable; do not leave unreachable! in production paths.
    unreachable!("standalone TCP connections are not constructed until TcpConnector wiring")
}

/// Passive read result for one TCP connection or reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TcpRead {
    /// No bytes or terminal state are currently available.
    Pending,
    /// The supplied destination received this many bytes.
    Data(usize),
    /// The reader has observed EOF after draining retained bytes.
    Eof,
}

/// A terminal TCP stream failure, cancellation, reset, or invalid local operation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum TcpStreamError {
    /// The live connection failed.
    Failed(Error),
    /// Explicit cancellation won the terminal race.
    Cancelled,
    /// The peer reset the connection.
    Reset,
    /// The requested reader or writer operation is invalid in its current mode or state.
    Operation(Error),
}

impl TcpStreamError {
    /// Returns the underlying NBReq error for failures and invalid operations.
    #[must_use]
    pub fn error(&self) -> Option<&Error> {
        match self {
            Self::Failed(error) | Self::Operation(error) => Some(error),
            Self::Cancelled | Self::Reset => None,
        }
    }
}

impl fmt::Display for TcpStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(error) => write!(formatter, "TCP connection failed: {error}"),
            Self::Cancelled => formatter.write_str("TCP connection was cancelled"),
            Self::Reset => formatter.write_str("TCP connection was reset"),
            Self::Operation(error) => write!(formatter, "TCP stream operation failed: {error}"),
        }
    }
}

impl StdError for TcpStreamError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.error().map(|error| error as &(dyn StdError + 'static))
    }
}

/// Stable reason a TCP send was not fully accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TcpSendErrorKind {
    /// The complete buffer would exceed the currently available queue space.
    WouldBlock,
    /// The buffer can never fit within this connection's configured queue window.
    ChunkTooLarge,
    /// Accepting the buffer would exceed the Engine-owned queued-byte ceiling.
    QueueLimitExceeded,
    /// The write half is no longer accepting data.
    Closed,
    /// The peer reset the connection.
    Reset,
    /// Explicit cancellation won the terminal race.
    Cancelled,
    /// The owning Engine has stopped.
    EngineStopped,
    /// Blocking send is unavailable for a manually driven Engine.
    WrongMode,
    /// Standalone TCP is not wired on this Engine.
    Unsupported,
}

/// A failed [`TcpConnection::try_send`], [`TcpConnection::send`], [`TcpWriter::try_send`], or
/// [`TcpWriter::send`] operation.
///
/// The unaccepted suffix remains owned by the caller.
#[derive(Debug)]
pub struct TcpSendError {
    kind: TcpSendErrorKind,
    remaining: Vec<u8>,
}

impl TcpSendError {
    fn unwired(remaining: Vec<u8>) -> Self {
        Self {
            kind: TcpSendErrorKind::Unsupported,
            remaining,
        }
    }

    /// Returns the stable reason the bytes were not accepted.
    #[must_use]
    pub fn kind(&self) -> TcpSendErrorKind {
        self.kind
    }

    /// Returns the unaccepted caller-owned suffix.
    #[must_use]
    pub fn into_remaining(self) -> Vec<u8> {
        self.remaining
    }
}

impl fmt::Display for TcpSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            TcpSendErrorKind::WouldBlock => "TCP send would block",
            TcpSendErrorKind::ChunkTooLarge => "TCP send exceeds the connection queue window",
            TcpSendErrorKind::QueueLimitExceeded => {
                "TCP send exceeds the Engine queued-byte ceiling"
            }
            TcpSendErrorKind::Closed => "TCP write half is closed",
            TcpSendErrorKind::Reset => "TCP connection was reset",
            TcpSendErrorKind::Cancelled => "TCP connection was cancelled",
            TcpSendErrorKind::EngineStopped => "the owning Engine has stopped",
            TcpSendErrorKind::WrongMode => "blocking TCP send requires a spawned Engine",
            TcpSendErrorKind::Unsupported => "standalone TCP connections are not available yet",
        })
    }
}

impl StdError for TcpSendError {}

/// Failure while finishing a TCP write half.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TcpFinishError {
    /// The write half is already closed.
    Closed(Error),
    /// The peer reset the connection.
    Reset,
    /// The owning Engine has stopped.
    EngineStopped,
    /// Blocking finish is unavailable for a manually driven Engine.
    WrongMode,
    /// The live connection failed in transport.
    Failed(Error),
    /// Explicit cancellation won the terminal race.
    Cancelled,
    /// Standalone TCP is not wired on this Engine.
    Unsupported(Error),
}

impl TcpFinishError {
    fn unwired() -> Self {
        Self::Unsupported(Error::new(
            ErrorKind::Unsupported,
            "standalone TCP connections are not available yet",
        ))
    }

    /// Returns the underlying NBReq error when one is present.
    #[must_use]
    pub fn error(&self) -> Option<&Error> {
        match self {
            Self::Closed(error) | Self::Failed(error) | Self::Unsupported(error) => Some(error),
            Self::Reset | Self::EngineStopped | Self::WrongMode | Self::Cancelled => None,
        }
    }
}

impl fmt::Display for TcpFinishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed(error) => write!(formatter, "TCP write half is closed: {error}"),
            Self::Reset => formatter.write_str("TCP connection was reset"),
            Self::EngineStopped => formatter.write_str("the owning Engine has stopped"),
            Self::WrongMode => formatter.write_str("blocking TCP finish requires a spawned Engine"),
            Self::Failed(error) => write!(formatter, "TCP connection failed: {error}"),
            Self::Cancelled => formatter.write_str("TCP connection was cancelled"),
            Self::Unsupported(error) => write!(formatter, "{error}"),
        }
    }
}

impl StdError for TcpFinishError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.error().map(|error| error as &(dyn StdError + 'static))
    }
}

fn stopped_error() -> Error {
    Error::new(
        ErrorKind::EngineStopped,
        "the owning Engine has stopped accepting work",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, EngineConfig};
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::time::Duration;

    #[test]
    fn hostname_builder_normalizes_name_and_rejects_port_zero() {
        let request = TcpConnectRequest::hostname("Example.COM.", 1234)
            .connect_timeout(Duration::from_secs(5))
            .read_inactivity_timeout(Duration::from_secs(30))
            .write_inactivity_timeout(Duration::from_secs(15))
            .build()
            .expect("hostname connect must build");
        match request.target() {
            TcpConnectTarget::Hostname { name, port } => {
                assert_eq!(name, "example.com");
                assert_eq!(*port, 1234);
            }
            other => panic!("expected hostname target, got {other:?}"),
        }
        assert_eq!(request.connect_timeout(), Some(Duration::from_secs(5)));
        assert_eq!(
            request.read_inactivity_timeout(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            request.write_inactivity_timeout(),
            Some(Duration::from_secs(15))
        );
        let error = TcpConnectRequest::hostname("example.com", 0)
            .build()
            .expect_err("port zero must fail at build");
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
    }

    #[test]
    fn literal_builder_rejects_port_zero_and_unspecified_destinations() {
        let ok = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
        TcpConnectRequest::literal(ok)
            .build()
            .expect("loopback literal must build");
        TcpConnectRequest::literal(SocketAddr::from((Ipv6Addr::LOCALHOST, 9)))
            .build()
            .expect("IPv6 loopback literal must build");

        for addr in [
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 80)),
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 80, 0, 0)),
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, 80)),
            "0.0.0.0:80".parse().expect("unspecified v4"),
            "[::]:80".parse().expect("unspecified v6"),
            "[::ffff:0.0.0.0]:80"
                .parse()
                .expect("mapped unspecified v4"),
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        ] {
            let error = TcpConnectRequest::literal(addr)
                .build()
                .expect_err("invalid literal must fail at build");
            assert_eq!(error.kind(), ErrorKind::InvalidRequest, "{addr}");
        }
    }

    #[test]
    fn unavailable_connects_reject_before_admission_and_do_not_run_callbacks() {
        let engine = Engine::with_backend(EngineConfig::spawned(), crate::backend::scaffold())
            .expect("scaffold Engine must construct");
        let tcp = engine.tcp_connector();
        let request = TcpConnectRequest::hostname("example.com", 9)
            .build()
            .expect("connect request must build");
        let before = engine.metrics();
        let ran = StdArc::new(AtomicBool::new(false));
        let flag = StdArc::clone(&ran);
        let start_error = tcp
            .start(request.clone(), move |_| {
                flag.store(true, AtomicOrdering::SeqCst);
            })
            .expect_err("F0 connect must be unavailable");
        assert_eq!(start_error.kind(), ErrorKind::Unsupported);
        assert!(!ran.load(AtomicOrdering::SeqCst));
        let execute_error = tcp
            .execute(request)
            .expect_err("F0 execute must be unavailable");
        match execute_error {
            ExecuteError::Submission(error) => assert_eq!(error.kind(), ErrorKind::Unsupported),
            other => panic!("execute must fail before acceptance: {other:?}"),
        }
        let after = engine.metrics();
        assert_eq!(before, after);
        assert_eq!(after.tcp_connects_accepted(), 0);
        assert_eq!(after.current().standalone_tcp_connections(), 0);
        engine.shutdown().expect("scaffold Engine must stop");
    }

    #[test]
    fn per_connection_queue_cannot_exceed_the_engine_ceiling() {
        let engine = Engine::with_backend(
            EngineConfig::spawned().with_max_tcp_queue_bytes_per_connection(8),
            crate::backend::scaffold(),
        )
        .expect("bounded scaffold Engine must construct");
        let request = TcpConnectRequest::literal(SocketAddr::from((Ipv4Addr::LOCALHOST, 9)))
            .send_queue_bytes(16)
            .build()
            .expect("over-ceiling request must still build");
        let error = engine
            .tcp_connector()
            .submit(request)
            .expect_err("over-ceiling send window must fail before admission");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.limit_kind(), Some(LimitKind::TcpQueueBytes));
        engine.shutdown().expect("bounded Engine must stop");
    }
}
