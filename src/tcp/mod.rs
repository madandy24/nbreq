//! Public cleartext TCP connector contract.
//!
//! [`TcpConnector`] is an Engine-issued capability ticket. Native Engines accept literal-address
//! connections on their existing reactor owner. Hostname connections remain unavailable until
//! their exact resolver-to-connect policy is wired. Engines without the native owner fail
//! [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported) before admission.

pub(crate) mod io;

use std::cell::Cell;
use std::error::Error as StdError;
use std::fmt;
use std::marker::PhantomData;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use crate::dns::normalize_dns_name;
use crate::registry::{Shared, TcpConnectCallback, TcpConnectState};
use crate::{Error, ErrorKind, ExecuteError, RequestId};

use io::TcpIoShared;

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
}

impl TcpConnector {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
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
        let callback: TcpConnectCallback = Box::new(callback);
        let accepted = self
            .shared
            .accept_tcp_connect(self.clone(), request, Some(callback))?;
        Ok(TcpConnectHandle::new(self.clone(), accepted.state.id()))
    }

    /// Submits a connect and returns its direct terminal-state waiter.
    pub fn submit(&self, request: TcpConnectRequest) -> Result<PendingTcpConnect, Error> {
        let accepted = self
            .shared
            .accept_tcp_connect(self.clone(), request, None)?;
        let handle = TcpConnectHandle::new(self.clone(), accepted.state.id());
        Ok(PendingTcpConnect::new(handle, accepted.state))
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
    /// Cancellation is idempotent for same-Engine terminal connects and live connections.
    pub fn cancel(&self, request_id: RequestId) -> Result<(), Error> {
        self.shared.cancel(request_id)
    }
}

/// Engine-bound control handle for one accepted connect operation.
#[derive(Clone, Debug)]
pub struct TcpConnectHandle {
    connector: TcpConnector,
    id: RequestId,
}

impl TcpConnectHandle {
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
    state: Arc<TcpConnectState>,
}

impl PendingTcpConnect {
    pub(crate) fn new(handle: TcpConnectHandle, state: Arc<TcpConnectState>) -> Self {
        Self { handle, state }
    }

    /// Returns a clone of the independent cancellation handle.
    #[must_use]
    pub fn handle(&self) -> TcpConnectHandle {
        self.handle.clone()
    }

    /// Returns whether canonical terminal state has been committed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state.is_terminal()
    }

    pub(crate) fn try_completion(&self) -> Option<TcpConnectCompletion> {
        self.state.try_completion()
    }

    pub(crate) fn issued_engine_id(&self) -> u64 {
        self.handle.id.engine
    }

    /// Waits for and returns the canonical terminal outcome.
    #[must_use]
    pub fn wait(self) -> TcpConnectCompletion {
        self.state.wait()
    }

    /// Waits locally without changing connect state or cancelling on timeout.
    #[must_use]
    pub fn wait_for(self, duration: Duration) -> TcpConnectWaitOutcome {
        match self.state.wait_for(duration) {
            Some(completion) => TcpConnectWaitOutcome::Completed(completion),
            None => TcpConnectWaitOutcome::TimedOut(self),
        }
    }
}

/// Cloneable cancellation handle for one live TCP connection.
#[derive(Clone, Debug)]
pub struct TcpConnectionHandle {
    connector: TcpConnector,
    id: RequestId,
}

impl TcpConnectionHandle {
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
/// aborts it. Drop after a successful [`Self::try_finish`] does not revoke the write-side request,
/// but dropping before reader EOF still aborts because the read side is abandoned. Dropping after
/// both directions are terminal is harmless.
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
    io: Arc<TcpIoShared>,
    handle: TcpConnectionHandle,
    live: bool,
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
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_shared(io: Arc<TcpIoShared>, handle: TcpConnectionHandle) -> Self {
        Self {
            io,
            handle,
            live: true,
            _not_sync: PhantomData,
        }
    }

    /// Returns an independent cancellation-only handle for this connection.
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        self.handle.clone()
    }

    /// Returns the local socket address.
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.io.local_addr()
    }

    /// Returns the peer socket address.
    pub fn peer_addr(&self) -> Result<SocketAddr, Error> {
        self.io.peer_addr()
    }

    /// Consumes the unsplit connection into unique reader and writer halves.
    #[must_use]
    pub fn split(mut self) -> (TcpReader, TcpWriter) {
        self.live = false;
        (
            TcpReader {
                io: Arc::clone(&self.io),
                handle: self.handle.clone(),
                _not_sync: PhantomData,
            },
            TcpWriter {
                io: Arc::clone(&self.io),
                handle: self.handle.clone(),
                _not_sync: PhantomData,
            },
        )
    }

    /// Attempts to read available bytes without blocking.
    pub fn try_read(&mut self, destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        self.io.try_read(destination)
    }

    /// Reads bytes on a spawned Engine. Manual Engines return `WrongMode` rather than driving.
    pub fn read(&mut self, destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        self.io.read(destination)
    }

    /// Attempts to queue owned bytes without blocking. Refused input is returned unchanged.
    pub fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.io.try_send(bytes)
    }

    /// Queues owned bytes on a spawned Engine, returning any unaccepted suffix.
    ///
    /// Manual Engines return [`TcpSendErrorKind::WrongMode`] rather than driving.
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.io.send(bytes)
    }

    /// Requests write half-close without blocking, then polls that request.
    ///
    /// The first successful call closes the write side to new sends and asks the owner to drain
    /// accepted output and then half-close. It returns [`TcpFinishStatus::Pending`] until every
    /// accepted byte has reached the socket and write shutdown has completed. Later calls only
    /// poll. This is valid in manual mode and never drives the Engine.
    pub fn try_finish(&mut self) -> Result<TcpFinishStatus, TcpFinishError> {
        self.io.try_finish()
    }

    /// Drains accepted output and then half-closes the write side on a spawned Engine.
    ///
    /// This takes `&mut self` so the unsplit reader remains usable after write-half-close. Manual
    /// Engines reject blocking finish with [`TcpFinishError::WrongMode`] rather than driving.
    pub fn finish(&mut self) -> Result<(), TcpFinishError> {
        self.io.finish()
    }

    /// Registers one write-half-close callback and requests drain-then-half-close.
    ///
    /// Successful registration consumes one shared callback-event permit until the callback
    /// returns. It never runs during resolver/reactor processing, on the spawned network owner, or
    /// under locks. Manual Engines may deliver it on the thread calling [`crate::Engine::drive`]
    /// after the safe network-processing pass; spawned Engines use the accepted callback-dispatch
    /// domain. Dropping the writer after success does not revoke the finish request. A second registration fails with
    /// [`ErrorKind::InvalidRequest`]. Callback-event exhaustion is [`ErrorKind::QueueFull`] and
    /// does not request finish if it was not already requested.
    pub fn finish_with<F>(&mut self, callback: F) -> Result<(), Error>
    where
        F: FnOnce(Result<(), TcpFinishError>) + Send + 'static,
    {
        self.io.finish_with(callback)
    }
}

impl Drop for TcpConnection {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        self.io.writer_dropped();
        self.io.reader_dropped();
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
    io: Arc<TcpIoShared>,
    handle: TcpConnectionHandle,
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
        self.handle.clone()
    }

    /// Attempts to read available bytes without blocking.
    pub fn try_read(&mut self, destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        self.io.try_read(destination)
    }

    /// Reads bytes on a spawned Engine. Manual Engines return `WrongMode` rather than driving.
    pub fn read(&mut self, destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        self.io.read(destination)
    }
}

impl Drop for TcpReader {
    fn drop(&mut self) {
        self.io.reader_dropped();
    }
}

/// Unique TCP sending half.
///
/// Dropping the writer before a successful [`Self::try_finish`] or [`Self::finish`] aborts the
/// connection. Drop after `try_finish` has returned [`TcpFinishStatus::Pending`] or
/// [`TcpFinishStatus::Finished`] does not revoke the request; the Engine owns completing drain and
/// half-close.
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
    io: Arc<TcpIoShared>,
    handle: TcpConnectionHandle,
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
        self.handle.clone()
    }

    /// Attempts to queue owned bytes without blocking. Refused input is returned unchanged.
    pub fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.io.try_send(bytes)
    }

    /// Queues owned bytes on a spawned Engine, returning any unaccepted suffix.
    ///
    /// Manual Engines return [`TcpSendErrorKind::WrongMode`] rather than driving.
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.io.send(bytes)
    }

    /// Requests write half-close without blocking, then polls that request.
    ///
    /// See [`TcpConnection::try_finish`].
    pub fn try_finish(&mut self) -> Result<TcpFinishStatus, TcpFinishError> {
        self.io.try_finish()
    }

    /// Drains accepted output and then half-closes the write side on a spawned Engine.
    ///
    /// Manual Engines reject blocking finish with [`TcpFinishError::WrongMode`] rather than
    /// driving internally.
    pub fn finish(&mut self) -> Result<(), TcpFinishError> {
        self.io.finish()
    }

    /// Registers one write-half-close callback and requests drain-then-half-close.
    ///
    /// See [`TcpConnection::finish_with`]. At most one callback belongs to the write half, even if
    /// the connection is split after registration.
    pub fn finish_with<F>(&mut self, callback: F) -> Result<(), Error>
    where
        F: FnOnce(Result<(), TcpFinishError>) + Send + 'static,
    {
        self.io.finish_with(callback)
    }
}

impl Drop for TcpWriter {
    fn drop(&mut self) {
        self.io.writer_dropped();
    }
}

/// Passive result of [`TcpConnection::try_finish`] or [`TcpWriter::try_finish`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TcpFinishStatus {
    /// The finish request is accepted, but accepted output has not yet reached write shutdown.
    Pending,
    /// Accepted output has reached the socket and the write half is shut down.
    Finished,
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
    pub(crate) fn new(kind: TcpSendErrorKind, remaining: Vec<u8>) -> Self {
        Self { kind, remaining }
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
            TcpSendErrorKind::Closed => "TCP write half is closed",
            TcpSendErrorKind::Reset => "TCP connection was reset",
            TcpSendErrorKind::Cancelled => "TCP connection was cancelled",
            TcpSendErrorKind::EngineStopped => "the owning Engine has stopped",
            TcpSendErrorKind::WrongMode => "blocking TCP send requires a spawned Engine",
            TcpSendErrorKind::Unsupported => {
                "standalone TCP connections are not available on this Engine"
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::CallbackJob;
    use crate::{Engine, EngineConfig};
    #[cfg(feature = "native")]
    use crate::{LimitKind, TimeoutKind};
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
    use std::num::NonZeroUsize;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

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
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        engine.shutdown().expect("bounded Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_literal_connect_is_duplex_on_the_existing_owner() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept standalone TCP");
            let mut bytes = [0_u8; 4];
            socket.read_exact(&mut bytes).expect("read request bytes");
            assert_eq!(&bytes, b"ping");
            socket.write_all(b"pong").expect("write response bytes");
        });

        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let request = TcpConnectRequest::literal(address)
            .connect_timeout(Duration::from_secs(2))
            .send_queue_bytes(16)
            .receive_queue_bytes(16)
            .build()
            .expect("literal request must build");
        let mut connection = engine
            .tcp_connector()
            .execute(request)
            .expect("literal connect must complete");
        assert_eq!(connection.peer_addr().expect("peer address"), address);
        connection.send(b"ping".to_vec()).expect("queue request");
        let mut response = [0_u8; 4];
        assert_eq!(
            connection.read(&mut response).expect("read response"),
            Some(4)
        );
        assert_eq!(&response, b"pong");
        drop(connection);
        server.join().expect("fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_read_inactivity_fails_a_silent_live_connection() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::thread;

        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind read-inactivity fixture");
        let address = listener.local_addr().expect("read-inactivity address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept silent TCP");
            let mut byte = [0_u8; 1];
            let _closed_or_reset = socket.read(&mut byte);
        });

        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let mut connection = engine
            .tcp_connector()
            .execute(
                TcpConnectRequest::literal(address)
                    .read_inactivity_timeout(Duration::from_millis(50))
                    .build()
                    .expect("read-inactivity request must build"),
            )
            .expect("silent TCP must connect");
        let cancel = connection.handle();
        let (result_tx, result_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut byte = [0_u8; 1];
            result_tx
                .send(connection.read(&mut byte))
                .expect("send read-inactivity result");
        });
        let result = match result_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => result,
            Err(error) => {
                cancel.cancel().expect("guard cancellation must succeed");
                panic!("read inactivity did not terminate the connection: {error}");
            }
        };
        assert!(matches!(
            result,
            Err(TcpStreamError::Failed(error))
                if error.kind() == ErrorKind::Timeout
                    && error.timeout_kind() == Some(TimeoutKind::Inactivity)
        ));
        assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
        assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
        server.join().expect("silent fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_read_progress_refreshes_inactivity() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind read-progress fixture");
        let address = listener.local_addr().expect("read-progress address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept read-progress TCP");
            socket.write_all(b"a").expect("write first progress byte");
            thread::sleep(Duration::from_millis(180));
            socket.write_all(b"b").expect("write second progress byte");
            thread::sleep(Duration::from_millis(180));
            socket.write_all(b"c").expect("write third progress byte");
            let mut byte = [0_u8; 1];
            let _closed_or_reset = socket.read(&mut byte);
        });

        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let mut connection = engine
            .tcp_connector()
            .execute(
                TcpConnectRequest::literal(address)
                    .receive_queue_bytes(8)
                    .read_inactivity_timeout(Duration::from_millis(300))
                    .build()
                    .expect("read-progress request must build"),
            )
            .expect("read-progress TCP must connect");
        let started = Instant::now();
        for expected in b"abc" {
            let mut byte = [0_u8; 1];
            assert_eq!(
                connection.read(&mut byte).expect("read progress byte"),
                Some(1)
            );
            assert_eq!(byte[0], *expected);
        }
        assert!(
            started.elapsed() > Duration::from_millis(300),
            "the fixture did not cross the original inactivity deadline"
        );
        drop(connection);
        server.join().expect("read-progress fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_receive_backpressure_pauses_then_restarts_read_inactivity() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind read-pressure fixture");
        let address = listener.local_addr().expect("read-pressure address");
        let (written_tx, written_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept read-pressure TCP");
            socket.write_all(b"abc").expect("fill receive window");
            written_tx.send(()).expect("signal filled receive window");
            let mut byte = [0_u8; 1];
            let _closed_or_reset = socket.read(&mut byte);
        });

        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let mut connection = engine
            .tcp_connector()
            .execute(
                TcpConnectRequest::literal(address)
                    .receive_queue_bytes(3)
                    .read_inactivity_timeout(Duration::from_millis(50))
                    .build()
                    .expect("read-pressure request must build"),
            )
            .expect("read-pressure TCP must connect");
        written_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fixture must write the receive window");
        thread::sleep(Duration::from_millis(150));

        let mut bytes = [0_u8; 3];
        assert_eq!(
            connection
                .try_read(&mut bytes)
                .expect("a full receive window must pause inactivity"),
            TcpRead::Data(3)
        );
        assert_eq!(&bytes, b"abc");

        let cancel = connection.handle();
        let (result_tx, result_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut byte = [0_u8; 1];
            result_tx
                .send(connection.read(&mut byte))
                .expect("send resumed-inactivity result");
        });
        let result = match result_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => result,
            Err(error) => {
                cancel.cancel().expect("guard cancellation must succeed");
                panic!("read inactivity did not restart after drain: {error}");
            }
        };
        assert!(matches!(
            result,
            Err(TcpStreamError::Failed(error))
                if error.kind() == ErrorKind::Timeout
                    && error.timeout_kind() == Some(TimeoutKind::Inactivity)
        ));
        assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
        assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
        server.join().expect("read-pressure fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_write_inactivity_fails_output_stalled_in_the_socket() {
        use std::net::TcpListener;
        use std::thread;

        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind write-inactivity fixture");
        let address = listener.local_addr().expect("write-inactivity address");
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (_socket, _) = listener.accept().expect("accept stalled writer TCP");
            release_rx.recv().expect("release stalled writer fixture");
        });

        let config = EngineConfig::manual();
        let backend = crate::backend::native_http_backend_with_write_limit(&config, 0)
            .expect("stalled-write backend must construct");
        let mut engine = Engine::with_backend(config, backend)
            .expect("manual standalone TCP Engine must construct");
        let pending = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::literal(address)
                    .send_queue_bytes(8)
                    .receive_queue_bytes(1)
                    .write_inactivity_timeout(Duration::from_millis(75))
                    .build()
                    .expect("write-inactivity request must build"),
            )
            .expect("stalled writer TCP must submit");
        let mut connection = match engine
            .drive_until(pending)
            .expect("stalled writer TCP must drive")
        {
            TcpConnectCompletion::Completed(connection) => connection,
            other => panic!("stalled writer TCP failed to connect: {other:?}"),
        };
        connection
            .try_send(vec![0xA5; 8])
            .expect("output must be accepted into the bounded window");
        assert_eq!(
            connection
                .try_finish()
                .expect("finish request must be accepted"),
            TcpFinishStatus::Pending
        );
        let started = Instant::now();
        let guard_deadline = Instant::now() + Duration::from_secs(2);
        let error = loop {
            let drive_deadline = (Instant::now() + Duration::from_millis(20)).min(guard_deadline);
            engine
                .drive(drive_deadline)
                .expect("manual stalled-write Engine must drive");
            match connection.try_finish() {
                Ok(TcpFinishStatus::Pending) => {
                    assert!(
                        Instant::now() < guard_deadline,
                        "write inactivity did not terminate the connection"
                    );
                }
                Ok(TcpFinishStatus::Finished) => {
                    panic!("stalled socket unexpectedly finished its write half")
                }
                Err(error) => break error,
            }
        };
        release_tx.send(()).expect("release stalled writer fixture");
        assert!(
            matches!(
                &error,
                TcpFinishError::Failed(error)
                    if error.kind() == ErrorKind::Timeout
                        && error.timeout_kind() == Some(TimeoutKind::Inactivity)
            ),
            "unexpected write-inactivity terminal: {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "write inactivity was not enforced promptly: {:?}",
            started.elapsed()
        );
        assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
        assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
        server.join().expect("write-inactivity fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_partial_write_progress_refreshes_inactivity_and_preserves_bytes() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind partial-write fixture");
        let address = listener.local_addr().expect("partial-write address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept partial writer TCP");
            let mut bytes = [0_u8; 8];
            socket
                .read_exact(&mut bytes)
                .expect("read every partial write");
            assert_eq!(&bytes, b"abcdefgh");
            socket.write_all(b"ok").expect("reply after partial writes");
        });

        let config = EngineConfig::manual();
        let backend = crate::backend::native_http_backend_with_write_limit(&config, 2)
            .expect("partial-write backend must construct");
        let mut engine = Engine::with_backend(config, backend)
            .expect("manual standalone TCP Engine must construct");
        let pending = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::literal(address)
                    .send_queue_bytes(8)
                    .receive_queue_bytes(2)
                    .write_inactivity_timeout(Duration::from_millis(200))
                    .build()
                    .expect("partial-write request must build"),
            )
            .expect("partial-write TCP must submit");
        let mut connection = match engine
            .drive_until(pending)
            .expect("partial-write TCP must drive")
        {
            TcpConnectCompletion::Completed(connection) => connection,
            other => panic!("partial-write TCP failed to connect: {other:?}"),
        };
        connection
            .try_send(b"abcdefgh".to_vec())
            .expect("partial output must enter the send window");
        assert_eq!(
            connection
                .try_finish()
                .expect("partial finish request must be accepted"),
            TcpFinishStatus::Pending
        );

        let guard_deadline = Instant::now() + Duration::from_secs(2);
        let mut pending_passes = 0;
        loop {
            engine
                .drive((Instant::now() + Duration::from_millis(20)).min(guard_deadline))
                .expect("partial-write Engine must drive");
            match connection
                .try_finish()
                .expect("partial progress must not time out")
            {
                TcpFinishStatus::Pending => {
                    pending_passes += 1;
                    assert!(
                        Instant::now() < guard_deadline,
                        "partial writes did not drain"
                    );
                    thread::sleep(Duration::from_millis(100));
                }
                TcpFinishStatus::Finished => break,
            }
        }
        assert!(
            pending_passes >= 3,
            "the two-byte test limit did not produce partial progress"
        );

        let mut reply = [0_u8; 2];
        loop {
            engine
                .drive((Instant::now() + Duration::from_millis(20)).min(guard_deadline))
                .expect("partial reply Engine must drive");
            match connection
                .try_read(&mut reply)
                .expect("partial writer reply must remain live")
            {
                TcpRead::Pending => assert!(
                    Instant::now() < guard_deadline,
                    "partial writer reply timed out"
                ),
                TcpRead::Data(2) => break,
                other => panic!("unexpected partial writer read: {other:?}"),
            }
        }
        assert_eq!(&reply, b"ok");
        drop(connection);
        server.join().expect("partial-write fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn finish_with_receives_write_inactivity_off_the_owner() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("bind finish-callback inactivity fixture");
        let address = listener
            .local_addr()
            .expect("finish-callback inactivity address");
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (_socket, _) = listener
                .accept()
                .expect("accept finish-callback inactivity TCP");
            release_rx
                .recv()
                .expect("release finish-callback inactivity fixture");
        });

        let config = EngineConfig::manual();
        let backend = crate::backend::native_http_backend_with_write_limit(&config, 0)
            .expect("stalled-write backend must construct");
        let mut engine = Engine::with_backend(config, backend)
            .expect("manual standalone TCP Engine must construct");
        let pending = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::literal(address)
                    .send_queue_bytes(4)
                    .receive_queue_bytes(1)
                    .write_inactivity_timeout(Duration::from_millis(60))
                    .build()
                    .expect("finish-callback request must build"),
            )
            .expect("finish-callback TCP must submit");
        let mut connection = match engine
            .drive_until(pending)
            .expect("finish-callback TCP must drive")
        {
            TcpConnectCompletion::Completed(connection) => connection,
            other => panic!("finish-callback TCP failed to connect: {other:?}"),
        };
        connection
            .try_send(b"full".to_vec())
            .expect("finish-callback output must queue");
        let (callback_tx, callback_rx) = mpsc::channel();
        connection
            .finish_with(move |result| {
                callback_tx
                    .send(result)
                    .expect("send finish-callback inactivity result");
            })
            .expect("finish callback must register");

        let guard_deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            engine
                .drive((Instant::now() + Duration::from_millis(20)).min(guard_deadline))
                .expect("finish-callback Engine must drive");
            match callback_rx.try_recv() {
                Ok(result) => break result,
                Err(mpsc::TryRecvError::Empty) => assert!(
                    Instant::now() < guard_deadline,
                    "finish callback did not receive inactivity terminal"
                ),
                Err(error) => panic!("finish callback channel disconnected: {error}"),
            }
        };
        assert!(matches!(
            result,
            Err(TcpFinishError::Failed(error))
                if error.kind() == ErrorKind::Timeout
                    && error.timeout_kind() == Some(TimeoutKind::Inactivity)
        ));
        assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
        assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
        release_tx
            .send(())
            .expect("release finish-callback inactivity fixture");
        drop(connection);
        server
            .join()
            .expect("finish-callback inactivity fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_peer_reset_is_not_flattened_into_a_generic_transport_failure() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind reset fixture");
        let address = listener.local_addr().expect("reset fixture address");
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (reset_tx, reset_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().expect("accept reset TCP");
            accepted_tx.send(()).expect("signal accepted reset TCP");
            reset_rx.recv().expect("release reset fixture");
            socket2::SockRef::from(&socket)
                .set_linger(Some(Duration::ZERO))
                .expect("configure abortive close");
            drop(socket);
        });

        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let mut connection = engine
            .tcp_connector()
            .execute(
                TcpConnectRequest::literal(address)
                    .build()
                    .expect("reset request must build"),
            )
            .expect("reset TCP must connect");
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server must accept reset TCP");
        reset_tx.send(()).expect("trigger abortive close");

        let cancel = connection.handle();
        let (result_tx, result_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut byte = [0_u8; 1];
            result_tx
                .send(connection.read(&mut byte))
                .expect("send reset result");
        });
        let result = match result_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => result,
            Err(error) => {
                cancel.cancel().expect("guard cancellation must succeed");
                panic!("peer reset did not terminate the connection: {error}");
            }
        };
        assert!(matches!(result, Err(TcpStreamError::Reset)));
        server.join().expect("reset fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_tcp_limits_apply_after_capability_and_before_admission() {
        let config = EngineConfig::spawned().with_max_tcp_queue_bytes_per_connection(8);
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let request = TcpConnectRequest::literal(SocketAddr::from((Ipv4Addr::LOCALHOST, 9)))
            .send_queue_bytes(16)
            .build()
            .expect("over-ceiling request must build");
        let error = engine
            .tcp_connector()
            .submit(request)
            .expect_err("over-ceiling send window must fail before admission");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.limit_kind(), Some(LimitKind::TcpQueueBytes));
        assert_eq!(engine.metrics().tcp_connects_accepted(), 0);
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn manual_literal_connect_uses_drive_and_cancel_before_drive_wins() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind manual fixture");
        let address = listener.local_addr().expect("manual fixture address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept manual TCP");
            let mut byte = [0_u8; 1];
            socket.read_exact(&mut byte).expect("read manual byte");
            socket.write_all(&byte).expect("echo manual byte");
        });

        let config = EngineConfig::manual();
        let backend = crate::backend::native_http_backend(&config)
            .expect("manual native backend must construct");
        let mut engine = Engine::with_backend(config, backend)
            .expect("manual standalone TCP Engine must construct");
        let connector = engine.tcp_connector();
        let cancelled = connector
            .submit(
                TcpConnectRequest::literal(address)
                    .connect_timeout(Duration::from_secs(2))
                    .build()
                    .expect("cancel request must build"),
            )
            .expect("cancel request must submit");
        cancelled
            .handle()
            .cancel()
            .expect("cancel must be accepted");
        assert!(matches!(
            engine
                .drive_until(cancelled)
                .expect("cancelled waiter must drive"),
            TcpConnectCompletion::Cancelled
        ));

        let pending = connector
            .submit(
                TcpConnectRequest::literal(address)
                    .connect_timeout(Duration::from_secs(2))
                    .send_queue_bytes(8)
                    .receive_queue_bytes(8)
                    .build()
                    .expect("manual request must build"),
            )
            .expect("manual request must submit");
        let mut connection = match engine.drive_until(pending).expect("connect must drive") {
            TcpConnectCompletion::Completed(connection) => connection,
            other => panic!("manual connect failed: {other:?}"),
        };
        connection
            .try_send(vec![7])
            .expect("manual byte must queue");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut byte = [0_u8; 1];
        loop {
            engine.drive(deadline).expect("manual Engine must drive");
            match connection
                .try_read(&mut byte)
                .expect("manual read must remain live")
            {
                TcpRead::Pending => assert!(Instant::now() < deadline, "manual echo timed out"),
                TcpRead::Data(1) => break,
                other => panic!("unexpected manual read: {other:?}"),
            }
        }
        assert_eq!(byte, [7]);
        drop(connection);
        server.join().expect("manual fixture must join");
        engine.shutdown().expect("manual Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn live_cancel_releases_tcp_occupancy_and_queue_reservation() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind cancel fixture");
        let address = listener.local_addr().expect("cancel fixture address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept cancelled TCP");
            let mut byte = [0_u8; 1];
            let _closed_or_reset = socket.read(&mut byte);
        });
        let config = EngineConfig::spawned().with_max_queued_bytes(16);
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let request = TcpConnectRequest::literal(address)
            .connect_timeout(Duration::from_secs(2))
            .send_queue_bytes(8)
            .receive_queue_bytes(8)
            .build()
            .expect("cancel request must build");
        let mut connection = engine
            .tcp_connector()
            .execute(request)
            .expect("literal connect must complete");
        let live = engine.metrics();
        assert_eq!(live.current().standalone_tcp_connections(), 1);
        assert_eq!(live.current().reserved_tcp_queue_bytes(), 16);
        connection
            .handle()
            .cancel()
            .expect("live cancel must succeed");
        let mut byte = [0_u8; 1];
        assert!(matches!(
            connection.try_read(&mut byte),
            Err(TcpStreamError::Cancelled)
        ));
        let released = engine.metrics();
        assert_eq!(released.current().standalone_tcp_connections(), 0);
        assert_eq!(released.current().reserved_tcp_queue_bytes(), 0);
        drop(connection);
        server.join().expect("cancel fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn live_cancel_and_split_half_drops_release_once_under_race() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::sync::Barrier;
        use std::thread;

        const REPEATS: usize = 25;
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind cancel/drop race fixture");
        let address = listener.local_addr().expect("cancel/drop race address");
        let server = thread::spawn(move || {
            for _ in 0..REPEATS {
                let (mut socket, _) = listener.accept().expect("accept raced TCP");
                let mut byte = [0_u8; 1];
                let _closed_or_reset = socket.read(&mut byte);
            }
        });

        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        for _ in 0..REPEATS {
            let connection = engine
                .tcp_connector()
                .execute(
                    TcpConnectRequest::literal(address)
                        .send_queue_bytes(4)
                        .receive_queue_bytes(4)
                        .build()
                        .expect("raced TCP request must build"),
                )
                .expect("raced TCP must connect");
            let handle = connection.handle();
            let (reader, writer) = connection.split();
            let barrier = StdArc::new(Barrier::new(4));
            let reader_barrier = StdArc::clone(&barrier);
            let reader_drop = thread::spawn(move || {
                reader_barrier.wait();
                drop(reader);
            });
            let writer_barrier = StdArc::clone(&barrier);
            let writer_drop = thread::spawn(move || {
                writer_barrier.wait();
                drop(writer);
            });
            let cancel_barrier = StdArc::clone(&barrier);
            let cancel = thread::spawn(move || {
                cancel_barrier.wait();
                handle.cancel().expect("raced cancel must be idempotent");
            });
            barrier.wait();
            reader_drop.join().expect("reader drop must join");
            writer_drop.join().expect("writer drop must join");
            cancel.join().expect("cancel must join");
            assert_eq!(
                engine.metrics().current().standalone_tcp_connections(),
                0,
                "raced release must happen exactly once"
            );
            assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);
        }
        server.join().expect("cancel/drop race fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn standalone_occupancy_covers_pending_and_live_then_releases_on_drop() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind occupancy fixture");
        let address = listener.local_addr().expect("occupancy fixture address");
        let server = thread::spawn(move || {
            let (first, _) = listener.accept().expect("accept first TCP session");
            drop(first);
            let (second, _) = listener.accept().expect("accept replacement TCP session");
            drop(second);
        });
        let config = EngineConfig::spawned()
            .with_max_standalone_tcp_connections(NonZeroUsize::new(1).expect("nonzero limit"));
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let connector = engine.tcp_connector();
        let request = || {
            TcpConnectRequest::literal(address)
                .connect_timeout(Duration::from_secs(2))
                .build()
                .expect("occupancy request must build")
        };

        let first = connector
            .execute(request())
            .expect("first TCP session must connect");
        let rejected = connector
            .submit(request())
            .expect_err("a live session must retain the sole occupancy permit");
        assert_eq!(rejected.kind(), ErrorKind::QueueFull);
        drop(first);
        assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);

        let replacement = connector
            .execute(request())
            .expect("dropping the first session must release occupancy");
        drop(replacement);
        server.join().expect("occupancy fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn shutdown_aborts_a_live_native_tcp_session_and_joins() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind shutdown fixture");
        let address = listener.local_addr().expect("shutdown fixture address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept shutdown TCP");
            let mut byte = [0_u8; 1];
            let _closed_or_reset = socket.read(&mut byte);
        });
        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let mut connection = engine
            .tcp_connector()
            .execute(
                TcpConnectRequest::literal(address)
                    .connect_timeout(Duration::from_secs(2))
                    .build()
                    .expect("shutdown request must build"),
            )
            .expect("shutdown TCP session must connect");

        engine.shutdown().expect("Engine with live TCP must join");
        let mut byte = [0_u8; 1];
        assert!(matches!(
            connection.try_read(&mut byte),
            Err(TcpStreamError::Failed(error)) if error.kind() == ErrorKind::EngineStopped
        ));
        drop(connection);
        server.join().expect("shutdown fixture must join");
    }

    #[cfg(feature = "native")]
    #[test]
    fn spawned_start_delivers_one_unique_connection_off_the_reactor() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind callback fixture");
        let address = listener.local_addr().expect("callback fixture address");
        let server = thread::spawn(move || {
            let (_socket, _) = listener.accept().expect("accept callback TCP");
        });
        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let (sent, received) = mpsc::channel();
        let handle = engine
            .tcp_connector()
            .start(
                TcpConnectRequest::literal(address)
                    .connect_timeout(Duration::from_secs(2))
                    .build()
                    .expect("callback request must build"),
                move |completion| {
                    sent.send(completion).expect("send callback completion");
                },
            )
            .expect("callback connect must start");
        let connection = match received
            .recv_timeout(Duration::from_secs(2))
            .expect("callback must arrive")
        {
            TcpConnectCompletion::Completed(connection) => connection,
            other => panic!("callback connect failed: {other:?}"),
        };
        assert_eq!(connection.handle().id(), handle.id());
        drop(connection);
        server.join().expect("callback fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_finish_drains_then_half_closes_without_losing_the_reader() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind finish fixture");
        let address = listener.local_addr().expect("finish fixture address");
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept finishing TCP");
            let mut request = Vec::new();
            socket
                .read_to_end(&mut request)
                .expect("observe client write FIN");
            assert_eq!(request, b"finished");
            socket.write_all(b"yes").expect("reply after client FIN");
        });
        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("native standalone TCP Engine must construct");
        let mut connection = engine
            .tcp_connector()
            .execute(
                TcpConnectRequest::literal(address)
                    .connect_timeout(Duration::from_secs(2))
                    .build()
                    .expect("finish request must build"),
            )
            .expect("finish connect must complete");
        connection
            .send(b"finished".to_vec())
            .expect("finish payload must queue");
        connection.finish().expect("write half must finish");
        let mut response = [0_u8; 3];
        assert_eq!(connection.read(&mut response).expect("read reply"), Some(3));
        assert_eq!(&response, b"yes");
        assert_eq!(connection.read(&mut response).expect("read EOF"), None);
        drop(connection);
        server.join().expect("finish fixture must join");
        engine.shutdown().expect("native Engine must stop");
    }

    #[cfg(feature = "native")]
    #[test]
    fn http_stream_and_tcp_share_the_parent_queue_budget() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::thread;

        let http_listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind HTTP budget fixture");
        let http_address = http_listener.local_addr().expect("HTTP fixture address");
        let (http_seen_tx, http_seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let http_server = thread::spawn(move || {
            let (mut socket, _) = http_listener.accept().expect("accept streaming HTTP");
            let mut request = [0_u8; 512];
            let _ = socket.read(&mut request).expect("read HTTP request");
            http_seen_tx.send(()).expect("signal HTTP admission");
            release_rx.recv().expect("release HTTP fixture");
        });
        let tcp_listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind TCP budget fixture");
        let tcp_address = tcp_listener.local_addr().expect("TCP fixture address");
        let tcp_server = thread::spawn(move || {
            let (_socket, _) = tcp_listener.accept().expect("accept budget TCP");
        });

        let config = EngineConfig::spawned()
            .with_max_stream_queue_bytes_per_request(8)
            .with_max_stream_queued_bytes(16)
            .with_max_tcp_queue_bytes_per_connection(8)
            .with_max_queued_bytes(16);
        let factory = crate::backend::native_http_factory(&config);
        let engine = Engine::with_spawned_factory(config, factory)
            .expect("shared-budget Engine must construct");
        let reader = engine
            .client()
            .submit_stream(
                crate::StreamRequest::get(format!("http://{http_address}/"))
                    .build()
                    .expect("stream request must build"),
            )
            .expect("HTTP stream must reserve eight bytes");
        http_seen_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("HTTP stream must reach fixture");
        let connection = engine
            .tcp_connector()
            .execute(
                TcpConnectRequest::literal(tcp_address)
                    .send_queue_bytes(4)
                    .receive_queue_bytes(4)
                    .build()
                    .expect("TCP request must build"),
            )
            .expect("TCP must consume the remaining eight bytes");
        let rejected = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::literal(tcp_address)
                    .send_queue_bytes(4)
                    .receive_queue_bytes(4)
                    .build()
                    .expect("second TCP request must build"),
            )
            .expect_err("shared parent budget must reject another reservation");
        assert_eq!(rejected.kind(), ErrorKind::Limit);
        assert_eq!(engine.metrics().tcp_connects_accepted(), 1);

        drop(connection);
        drop(reader);
        release_tx.send(()).expect("release HTTP fixture");
        tcp_server.join().expect("TCP budget fixture must join");
        http_server.join().expect("HTTP budget fixture must join");
        engine.shutdown().expect("shared-budget Engine must stop");
    }

    struct Live {
        occupancy: StdArc<std::sync::atomic::AtomicUsize>,
        reserved: StdArc<std::sync::atomic::AtomicUsize>,
        engine: Engine,
        connection: TcpConnection,
        /// Last field so the fake owner outlives the public handle.
        /// Struct fields drop in declaration order.
        owner: super::io::TcpIoOwner,
    }

    fn live_tcp(mode: crate::RunMode, send: usize, recv: usize) -> Live {
        let config = match mode {
            crate::RunMode::Spawned => EngineConfig::spawned(),
            crate::RunMode::Manual => EngineConfig::manual(),
        };
        live_tcp_cfg(config, send, recv)
    }

    fn live_tcp_cfg(config: EngineConfig, send: usize, recv: usize) -> Live {
        let mode = config.run_mode();
        let engine = Engine::with_backend(config, crate::backend::scaffold())
            .expect("scaffold Engine must construct");
        let attached = attach_live(&engine, mode, send, recv, 1);
        Live {
            occupancy: attached.occupancy,
            reserved: attached.reserved,
            engine,
            connection: attached.connection,
            owner: attached.owner,
        }
    }

    struct Attached {
        occupancy: StdArc<std::sync::atomic::AtomicUsize>,
        reserved: StdArc<std::sync::atomic::AtomicUsize>,
        connection: TcpConnection,
        owner: super::io::TcpIoOwner,
    }

    fn attach_live(
        engine: &Engine,
        mode: crate::RunMode,
        send: usize,
        recv: usize,
        sequence: u64,
    ) -> Attached {
        use super::io::{TcpIoConfig, TcpIoShared};
        let connector = engine.tcp_connector();
        let id = RequestId {
            engine: connector.shared.id,
            sequence,
        };
        let handle = TcpConnectionHandle::new(connector.clone(), id);
        let occupancy = StdArc::new(std::sync::atomic::AtomicUsize::new(1));
        let reserved = StdArc::new(std::sync::atomic::AtomicUsize::new(send + recv));
        let occ = StdArc::clone(&occupancy);
        let res = StdArc::clone(&reserved);
        let (io, owner) = TcpIoShared::pair(TcpIoConfig {
            engine_id: id.engine,
            request_id: id,
            shared: StdArc::clone(&connector.shared),
            run_mode: mode,
            send_window: send,
            receive_window: recv,
            local: SocketAddr::from((Ipv4Addr::LOCALHOST, 1)),
            peer: SocketAddr::from((Ipv4Addr::LOCALHOST, 2)),
            engine_waker: Some(StdArc::new(|| {})),
            on_release: Box::new(move || {
                occ.store(0, AtomicOrdering::SeqCst);
                res.store(0, AtomicOrdering::SeqCst);
            }),
        });
        Attached {
            occupancy,
            reserved,
            connection: TcpConnection::from_shared(io, handle),
            owner,
        }
    }

    fn recv_finish(rx: &mpsc::Receiver<Result<(), TcpFinishError>>) -> Result<(), TcpFinishError> {
        rx.recv_timeout(Duration::from_secs(2))
            .expect("finish callback must run")
    }

    #[test]
    fn try_send_preserves_refused_bytes_and_distinguishes_window_errors() {
        let mut live = live_tcp(crate::RunMode::Manual, 4, 4);
        let too_big = live
            .connection
            .try_send(b"hello".to_vec())
            .expect_err("oversize chunk cannot fit the window");
        assert_eq!(too_big.kind(), TcpSendErrorKind::ChunkTooLarge);
        assert_eq!(too_big.into_remaining(), b"hello");

        live.connection
            .try_send(b"abcd".to_vec())
            .expect("full window can be queued");
        let blocked = live
            .connection
            .try_send(b"x".to_vec())
            .expect_err("occupied window refuses another byte");
        assert_eq!(blocked.kind(), TcpSendErrorKind::WouldBlock);
        assert_eq!(blocked.into_remaining(), b"x");
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn staging_into_the_pump_does_not_release_send_window_capacity() {
        let mut live = live_tcp(crate::RunMode::Manual, 8, 4);
        live.connection
            .try_send(b"abcd".to_vec())
            .expect("first chunk fits");
        let staged = live
            .owner
            .take_outbound()
            .expect("owner takes queued bytes");
        assert_eq!(staged, b"abcd");
        assert_eq!(live.owner.outbound_bytes(), 0);
        assert_eq!(live.owner.pump_bytes(), 4);
        assert_eq!(live.owner.send_occupancy(), 4);

        live.connection
            .try_send(b"efgh".to_vec())
            .expect("remaining window still accounts for pump bytes");
        let blocked = live
            .connection
            .try_send(b"x".to_vec())
            .expect_err("pump occupancy still consumes the window");
        assert_eq!(blocked.kind(), TcpSendErrorKind::WouldBlock);

        live.owner.write_progress(4);
        live.connection
            .try_send(b"ijkl".to_vec())
            .expect("socket progress frees window capacity");
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn try_finish_is_request_once_and_later_send_is_closed() {
        let mut live = live_tcp(crate::RunMode::Manual, 8, 4);
        live.connection
            .try_send(b"ab".to_vec())
            .expect("bytes queue");
        assert_eq!(
            live.connection.try_finish().expect("finish request"),
            TcpFinishStatus::Pending
        );
        assert!(live.owner.finish_requested());
        assert_eq!(
            live.connection.try_finish().expect("later poll"),
            TcpFinishStatus::Pending
        );
        let closed = live
            .connection
            .try_send(b"cd".to_vec())
            .expect_err("send after finish request is closed");
        assert_eq!(closed.kind(), TcpSendErrorKind::Closed);
        assert_eq!(closed.into_remaining(), b"cd");

        let drained = live.owner.take_outbound().expect("drain accepted output");
        assert_eq!(drained, b"ab");
        live.owner.write_progress(2);
        live.owner
            .complete_write_shutdown()
            .expect("owner completes half-close");
        assert_eq!(
            live.connection.try_finish().expect("completed finish"),
            TcpFinishStatus::Finished
        );
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn writer_drop_before_finish_request_aborts_and_releases_once() {
        let mut live = live_tcp(crate::RunMode::Manual, 4, 4);
        live.connection.try_send(b"ab".to_vec()).expect("queued");
        drop(live.connection);
        assert_eq!(live.occupancy.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(live.reserved.load(AtomicOrdering::SeqCst), 0);
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn writer_drop_after_pending_finish_does_not_revoke_the_request() {
        let mut live = live_tcp(crate::RunMode::Manual, 8, 4);
        live.connection.try_send(b"ab".to_vec()).expect("queued");
        let (mut reader, mut writer) = live.connection.split();
        assert_eq!(
            writer.try_finish().expect("request finish"),
            TcpFinishStatus::Pending
        );
        drop(writer);
        assert_eq!(live.occupancy.load(AtomicOrdering::SeqCst), 1);

        let drained = live.owner.take_outbound().expect("engine still drains");
        assert_eq!(drained, b"ab");
        live.owner.write_progress(2);
        live.owner
            .complete_write_shutdown()
            .expect("half-close completes after writer drop");
        live.owner.peer_closed();
        assert_eq!(reader.try_read(&mut [0; 8]).expect("eof"), TcpRead::Eof);
        assert_eq!(live.occupancy.load(AtomicOrdering::SeqCst), 0);
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn unsplit_drop_while_finishing_without_eof_still_aborts() {
        let mut live = live_tcp(crate::RunMode::Manual, 8, 4);
        live.connection.try_send(b"ab".to_vec()).expect("queued");
        live.connection
            .try_finish()
            .expect("write finish requested");
        drop(live.connection);
        assert_eq!(live.occupancy.load(AtomicOrdering::SeqCst), 0);
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn reader_drains_retained_bytes_before_eof_then_drop_is_harmless() {
        let mut live = live_tcp(crate::RunMode::Manual, 4, 8);
        live.owner
            .push_inbound(b"hi".to_vec())
            .expect("inbound fits");
        live.owner.peer_closed();
        let mut buf = [0; 8];
        assert_eq!(
            live.connection.try_read(&mut buf).expect("data"),
            TcpRead::Data(2)
        );
        assert_eq!(&buf[..2], b"hi");
        assert_eq!(
            live.connection.try_read(&mut buf).expect("eof"),
            TcpRead::Eof
        );
        live.connection
            .try_finish()
            .expect("finish empty write side");
        live.owner
            .complete_write_shutdown()
            .expect("empty write shutdown");
        assert_eq!(live.occupancy.load(AtomicOrdering::SeqCst), 0);
        drop(live.connection);
        assert_eq!(live.occupancy.load(AtomicOrdering::SeqCst), 0);
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn blocking_finish_and_send_reject_manual_mode_without_consuming_bytes() {
        let mut live = live_tcp(crate::RunMode::Manual, 8, 4);
        let error = live
            .connection
            .send(b"ab".to_vec())
            .expect_err("manual send is WrongMode");
        assert_eq!(error.kind(), TcpSendErrorKind::WrongMode);
        assert_eq!(error.into_remaining(), b"ab");
        match live.connection.finish() {
            Err(TcpFinishError::WrongMode) => {}
            other => panic!("expected WrongMode, got {other:?}"),
        }
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn spawned_blocking_finish_waits_for_owner_shutdown() {
        let mut live = live_tcp(crate::RunMode::Spawned, 8, 4);
        live.connection.try_send(b"xy".to_vec()).expect("queued");
        let (mut reader, mut writer) = live.connection.split();
        let join = std::thread::spawn(move || writer.finish());
        let drained = loop {
            if let Some(chunk) = live.owner.take_outbound() {
                break chunk;
            }
            std::thread::yield_now();
        };
        assert_eq!(drained, b"xy");
        live.owner.write_progress(2);
        while !live.owner.finish_requested() {
            std::thread::yield_now();
        }
        live.owner
            .complete_write_shutdown()
            .expect("owner completes finish");
        join.join()
            .expect("finish thread")
            .expect("spawned finish completes");
        live.owner.peer_closed();
        assert_eq!(
            reader.try_read(&mut [0; 4]).expect("eof after peer FIN"),
            TcpRead::Eof
        );
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn reset_and_failure_release_once_and_read_allowance_tracks_unread_bytes() {
        let mut live = live_tcp(crate::RunMode::Manual, 4, 8);
        assert_eq!(live.owner.read_allowance(), 8);
        live.owner
            .push_inbound(b"xy".to_vec())
            .expect("inbound fits");
        assert_eq!(live.owner.read_allowance(), 6);
        live.owner.reset();
        let reset = live
            .connection
            .try_send(b"ab".to_vec())
            .expect_err("reset closes send");
        assert_eq!(reset.kind(), TcpSendErrorKind::Reset);
        assert!(live.owner.session_released());
        assert_eq!(live.occupancy.load(AtomicOrdering::SeqCst), 0);

        let mut failed = live_tcp(crate::RunMode::Manual, 4, 4);
        failed
            .owner
            .fail(Error::new(ErrorKind::Internal, "injected owner failure"));
        match failed.connection.try_read(&mut [0; 1]) {
            Err(TcpStreamError::Failed(error)) => {
                assert_eq!(error.kind(), ErrorKind::Internal);
            }
            other => panic!("expected failed read, got {other:?}"),
        }
        assert!(failed.owner.session_released());
        failed.engine.shutdown().expect("Engine must stop");
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn finish_with_queue_full_does_not_request_finish() {
        let one = NonZeroUsize::MIN;
        let engine = Engine::with_backend(
            EngineConfig::spawned().with_callback_queue_capacity(one),
            crate::backend::scaffold(),
        )
        .expect("scaffold Engine must construct");
        let mut first = attach_live(&engine, crate::RunMode::Spawned, 8, 4, 1);
        let mut second = attach_live(&engine, crate::RunMode::Spawned, 8, 4, 2);
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        first
            .connection
            .finish_with(move |_| {
                let _ = hold_rx.recv();
            })
            .expect("first finish callback reserves capacity");
        second
            .connection
            .try_send(b"ab".to_vec())
            .expect("second connection can still send");
        let error = second
            .connection
            .finish_with(|_| panic!("rejected callback must not run"))
            .expect_err("callback capacity is exhausted");
        assert_eq!(error.kind(), ErrorKind::QueueFull);
        second
            .connection
            .try_send(b"cd".to_vec())
            .expect("QueueFull must not close the write half");
        drop(hold_tx);
        first
            .owner
            .complete_write_shutdown()
            .expect("first finish can complete");
        engine.shutdown().expect("Engine must stop");
        drop(second);
        drop(first);
    }

    #[test]
    fn finish_with_queue_full_after_try_finish_does_not_revoke() {
        let one = NonZeroUsize::MIN;
        let engine = Engine::with_backend(
            EngineConfig::spawned().with_callback_queue_capacity(one),
            crate::backend::scaffold(),
        )
        .expect("scaffold Engine must construct");
        let mut first = attach_live(&engine, crate::RunMode::Spawned, 4, 4, 1);
        let mut second = attach_live(&engine, crate::RunMode::Spawned, 4, 4, 2);
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        first
            .connection
            .finish_with(move |_| {
                let _ = hold_rx.recv();
            })
            .expect("first finish callback reserves capacity");
        second
            .connection
            .try_finish()
            .expect("second write finish requested without a callback");
        let error = second
            .connection
            .finish_with(|_| panic!("rejected callback must not run"))
            .expect_err("callback capacity is exhausted");
        assert_eq!(error.kind(), ErrorKind::QueueFull);
        let closed = second
            .connection
            .try_send(b"x".to_vec())
            .expect_err("existing finish request stays");
        assert_eq!(closed.kind(), TcpSendErrorKind::Closed);
        drop(hold_tx);
        first
            .owner
            .complete_write_shutdown()
            .expect("first finish can complete");
        second
            .owner
            .complete_write_shutdown()
            .expect("second finish request was not revoked");
        engine.shutdown().expect("Engine must stop");
        drop(second);
        drop(first);
    }

    #[test]
    fn finish_with_late_registration_after_finished() {
        let mut live = live_tcp(crate::RunMode::Spawned, 4, 4);
        live.connection
            .try_finish()
            .expect("request empty write finish");
        live.owner
            .complete_write_shutdown()
            .expect("owner completes finish");
        let (tx, rx) = mpsc::channel();
        live.connection
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("late registration after Finished");
        recv_finish(&rx).expect("late callback observes success");
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn finish_with_rejects_a_second_callback() {
        let mut live = live_tcp(crate::RunMode::Spawned, 4, 4);
        let (tx, rx) = mpsc::channel();
        live.connection
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("first callback");
        let (_reader, mut writer) = live.connection.split();
        let error = writer
            .finish_with(|_| panic!("second callback must not run"))
            .expect_err("only one finish callback even after split");
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        live.owner
            .complete_write_shutdown()
            .expect("owner completes finish");
        recv_finish(&rx).expect("first callback still runs");
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn finish_with_survives_writer_drop_after_registration() {
        let mut live = live_tcp(crate::RunMode::Spawned, 8, 4);
        live.connection.try_send(b"ab".to_vec()).expect("queued");
        let (mut reader, mut writer) = live.connection.split();
        let (tx, rx) = mpsc::channel();
        writer
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("register finish callback");
        drop(writer);
        assert_eq!(live.occupancy.load(AtomicOrdering::SeqCst), 1);
        let drained = live.owner.take_outbound().expect("engine still drains");
        assert_eq!(drained, b"ab");
        live.owner.write_progress(2);
        live.owner
            .complete_write_shutdown()
            .expect("half-close completes after writer drop");
        recv_finish(&rx).expect("callback still runs");
        live.owner.peer_closed();
        assert_eq!(reader.try_read(&mut [0; 8]).expect("eof"), TcpRead::Eof);
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn finish_with_unsplit_drop_delivers_cancelled() {
        let mut live = live_tcp(crate::RunMode::Spawned, 4, 4);
        let (tx, rx) = mpsc::channel();
        live.connection
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("register finish callback");
        drop(live.connection);
        match recv_finish(&rx) {
            Err(TcpFinishError::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn finish_with_manual_delivery_waits_for_drive() {
        let mut live = live_tcp(crate::RunMode::Manual, 4, 4);
        let (tx, rx) = mpsc::channel();
        live.connection
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("register finish callback");
        live.owner
            .complete_write_shutdown()
            .expect("owner completes finish");
        assert!(
            rx.try_recv().is_err(),
            "manual finish callback must wait for drive"
        );
        live.engine
            .drive(Instant::now())
            .expect("manual drive delivers callbacks");
        recv_finish(&rx).expect("callback runs during drive");
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn finish_with_stop_and_reset_and_failure_terminals() {
        let mut stopped = live_tcp(crate::RunMode::Spawned, 4, 4);
        let (tx, rx) = mpsc::channel();
        stopped
            .connection
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("register");
        drop(stopped.owner);
        match recv_finish(&rx) {
            Err(TcpFinishError::EngineStopped) => {}
            other => panic!("expected EngineStopped, got {other:?}"),
        }
        stopped.engine.shutdown().expect("Engine must stop");

        let mut reset = live_tcp(crate::RunMode::Spawned, 4, 4);
        let (tx, rx) = mpsc::channel();
        reset
            .connection
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("register");
        reset.owner.reset();
        match recv_finish(&rx) {
            Err(TcpFinishError::Reset) => {}
            other => panic!("expected Reset, got {other:?}"),
        }
        reset.engine.shutdown().expect("Engine must stop");

        let mut failed = live_tcp(crate::RunMode::Spawned, 4, 4);
        let (tx, rx) = mpsc::channel();
        failed
            .connection
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("register");
        failed.owner.fail(Error::new(
            ErrorKind::Transport,
            "injected transport failure",
        ));
        match recv_finish(&rx) {
            Err(TcpFinishError::Failed(error)) => {
                assert_eq!(error.kind(), ErrorKind::Transport);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        failed.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn finish_with_panic_does_not_kill_dispatcher() {
        let engine = Engine::with_backend(EngineConfig::spawned(), crate::backend::scaffold())
            .expect("scaffold Engine must construct");
        let mut panicking = attach_live(&engine, crate::RunMode::Spawned, 4, 4, 1);
        let mut survivor = attach_live(&engine, crate::RunMode::Spawned, 4, 4, 2);
        panicking
            .connection
            .finish_with(|_| panic!("finish callback panic must be contained"))
            .expect("panicking callback registers");
        panicking
            .owner
            .complete_write_shutdown()
            .expect("panicking finish completes");
        let (tx, rx) = mpsc::channel();
        survivor
            .connection
            .finish_with(move |result| {
                let _ = tx.send(result);
            })
            .expect("survivor callback registers");
        survivor
            .owner
            .complete_write_shutdown()
            .expect("survivor finish completes");
        recv_finish(&rx).expect("dispatcher survives finish-callback panic");
        engine.shutdown().expect("Engine must stop");
        drop(survivor);
        drop(panicking);
    }

    #[test]
    fn finish_with_from_same_request_callback_is_serialized() {
        let live = live_tcp(crate::RunMode::Spawned, 4, 4);
        let shared = live.engine.shared_for_testing();
        let id = live.connection.handle().id();
        let connection = std::sync::Mutex::new(live.connection);
        let owner = std::sync::Mutex::new(live.owner);
        let (running_tx, running_rx) = mpsc::channel();
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        let (finish_tx, finish_rx) = mpsc::channel();
        shared.enqueue_callback_job(CallbackJob::new(id, move || {
            connection
                .lock()
                .expect("connection lock")
                .finish_with(move |result| {
                    let _ = finish_tx.send(result);
                })
                .expect("finish_with from the in-flight RequestId callback");
            owner
                .lock()
                .expect("owner lock")
                .complete_write_shutdown()
                .expect("complete finish while the RequestId callback is still running");
            running_tx.send(()).expect("test receiver");
            let _ = hold_rx.recv();
        }));
        running_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("connect-callback analogue must call finish_with");
        assert!(
            finish_rx.try_recv().is_err(),
            "finish callback must not run until the RequestId callback returns"
        );
        drop(hold_tx);
        recv_finish(&finish_rx).expect("finish callback runs after return");
        live.engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn finish_with_admission_and_activation_are_atomic_against_shutdown() {
        let live = live_tcp(crate::RunMode::Spawned, 4, 4);
        let shared = live.engine.shared_for_testing();
        let (activation_tx, activation_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        shared.set_callback_activation_hook(move || {
            activation_tx
                .send(())
                .expect("test observes callback activation");
            release_rx
                .recv()
                .expect("test releases callback activation");
        });

        let Live {
            occupancy,
            reserved,
            engine,
            mut connection,
            owner,
        } = live;
        let (finish_tx, finish_rx) = mpsc::channel();
        let finish_thread = std::thread::spawn(move || {
            connection.finish_with(move |result| {
                let _ = finish_tx.send(result);
            })
        });
        activation_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("finish_with pauses after atomic admission and activation");

        let shutdown_shared = StdArc::clone(&shared);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let shutdown_thread = std::thread::spawn(move || {
            let _ = shutdown_tx.send(engine.shutdown());
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !shutdown_shared.stopped.load(AtomicOrdering::Acquire) {
            assert!(Instant::now() < deadline, "shutdown must begin");
            std::thread::yield_now();
        }
        assert!(
            shutdown_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "shutdown must wait for the activated callback registration"
        );

        drop(owner);
        release_tx.send(()).expect("release finish_with activation");
        finish_thread
            .join()
            .expect("finish_with thread")
            .expect("finish_with registration succeeds");
        match recv_finish(&finish_rx) {
            Err(TcpFinishError::EngineStopped) => {}
            other => panic!("owner stop must win the callback terminal, got {other:?}"),
        }
        shutdown_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("shutdown completes after activation")
            .expect("Engine shuts down cleanly");
        shutdown_thread.join().expect("shutdown thread");
        assert_eq!(occupancy.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(reserved.load(AtomicOrdering::SeqCst), 0);
    }
}
