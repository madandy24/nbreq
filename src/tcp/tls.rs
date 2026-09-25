//! Verified client TLS over the Engine-owned standalone TCP socket.

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use crate::dns::normalize_dns_name;
use crate::registry::{TlsConnectCallback, TlsConnectState};
use crate::{Error, ErrorKind, ExecuteError, RequestId, RunMode};

use super::{
    TcpConnectRequest, TcpConnection, TcpConnectionHandle, TcpConnector, TcpFinishError,
    TcpFinishStatus, TcpRead, TcpReader, TcpSendError, TcpStreamError, TcpWriter,
};

/// TLS peer identity and establishment policy. Certificate verification is always enabled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsOptions {
    server_name: String,
    handshake_timeout: Duration,
}

impl TlsOptions {
    /// Selects the name or IP address whose certificate must be verified.
    pub fn new(server_name: impl Into<String>) -> Result<Self, Error> {
        let server_name = server_name.into();
        let server_name = if let Ok(address) = server_name.parse::<IpAddr>() {
            address.to_string()
        } else {
            normalize_dns_name(&server_name)?.identity
        };
        if server_name.parse::<IpAddr>().is_err() && !valid_tls_dns_name(&server_name) {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "TLS server name is not a valid DNS identity or IP address",
            ));
        }
        Ok(Self {
            server_name,
            handshake_timeout: Duration::from_secs(10),
        })
    }

    /// Sets the finite deadline for DNS, TCP, and TLS establishment, or for an upgrade.
    #[must_use]
    pub fn handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Returns the certificate identity used for verification.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Returns the establishment timeout selected for this connection.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.handshake_timeout
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.handshake_timeout.is_zero() {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "TLS handshake timeout must be greater than zero",
            ));
        }
        if std::time::Instant::now()
            .checked_add(self.handshake_timeout)
            .is_none()
        {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "TLS handshake timeout cannot be represented by the Engine clock",
            ));
        }
        Ok(())
    }
}

fn valid_tls_dns_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 253 {
        return false;
    }
    let mut labels = name.split('.').peekable();
    while let Some(label) = labels.next() {
        if label.is_empty()
            || label.len() > 63
            || !label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return false;
        }
        if labels.peek().is_none() && label.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    true
}

/// Independent cancellation handle for a pending TLS connect or live TLS connection.
#[derive(Clone, Debug)]
pub struct TlsConnectHandle {
    connector: TcpConnector,
    id: RequestId,
}

impl TlsConnectHandle {
    pub(crate) fn new(connector: TcpConnector, id: RequestId) -> Self {
        Self { connector, id }
    }

    /// Returns the original TCP request identity, preserved across an upgrade.
    #[must_use]
    pub fn id(&self) -> RequestId {
        self.id
    }

    /// Cancels pending TLS establishment or aborts the live TLS connection.
    pub fn cancel(&self) -> Result<(), Error> {
        self.connector.cancel(self.id)
    }
}

/// Canonical outcome of either an immediate TLS connect or a consuming upgrade.
#[derive(Debug)]
#[non_exhaustive]
pub enum TlsConnectCompletion {
    /// A verified TLS connection is ready for application I/O.
    Completed(TlsConnection),
    /// DNS, TCP, or TLS establishment failed.
    Failed(Error),
    /// Cancellation won the terminal race.
    Cancelled,
}

/// Result of a timed wait for TLS establishment.
#[derive(Debug)]
#[non_exhaustive]
pub enum TlsConnectWaitOutcome {
    /// TLS establishment reached its canonical terminal outcome.
    Completed(TlsConnectCompletion),
    /// The wait elapsed; this handle can be waited on again or cancelled.
    TimedOut(PendingTlsConnect),
}

/// Accepted TLS establishment with a direct terminal-state waiter.
#[derive(Debug)]
pub struct PendingTlsConnect {
    handle: TlsConnectHandle,
    state: Arc<TlsConnectState>,
}

impl PendingTlsConnect {
    pub(crate) fn new(handle: TlsConnectHandle, state: Arc<TlsConnectState>) -> Self {
        Self { handle, state }
    }

    /// Returns a separate handle for cancellation and request identity.
    #[must_use]
    pub fn handle(&self) -> TlsConnectHandle {
        self.handle.clone()
    }

    /// Reports whether a terminal outcome has committed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state.is_terminal()
    }

    pub(crate) fn try_completion(&self) -> Option<TlsConnectCompletion> {
        self.state.try_completion()
    }

    pub(crate) fn issued_engine_id(&self) -> u64 {
        self.handle.id.engine
    }

    /// Waits for the terminal outcome. A manual Engine needs another driver to make progress.
    #[must_use]
    pub fn wait(self) -> TlsConnectCompletion {
        self.state.wait()
    }

    /// Waits up to `duration`, returning the pending handle when the wait expires.
    #[must_use]
    pub fn wait_for(self, duration: Duration) -> TlsConnectWaitOutcome {
        match self.state.wait_for(duration) {
            Some(completion) => TlsConnectWaitOutcome::Completed(completion),
            None => TlsConnectWaitOutcome::TimedOut(self),
        }
    }
}

/// Unique verified TLS stream. The underlying socket cannot be extracted or downgraded.
pub struct TlsConnection {
    inner: TcpConnection,
    _not_sync: PhantomData<Cell<()>>,
}

impl fmt::Debug for TlsConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsConnection")
            .finish_non_exhaustive()
    }
}

impl TlsConnection {
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) fn from_tcp(inner: TcpConnection) -> Self {
        Self {
            inner,
            _not_sync: PhantomData,
        }
    }

    /// Returns the connection's cancellation handle and original request identity.
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        self.inner.handle()
    }
    /// Returns the local socket address.
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.inner.local_addr()
    }
    /// Returns the connected peer address.
    pub fn peer_addr(&self) -> Result<SocketAddr, Error> {
        self.inner.peer_addr()
    }
    /// Splits the verified stream into independently owned read and write halves.
    #[must_use]
    pub fn split(self) -> (TlsReader, TlsWriter) {
        let (reader, writer) = self.inner.split();
        (
            TlsReader {
                inner: reader,
                _not_sync: PhantomData,
            },
            TlsWriter {
                inner: writer,
                _not_sync: PhantomData,
            },
        )
    }
    /// Reads available authenticated plaintext without blocking.
    pub fn try_read(&mut self, destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        self.inner.try_read(destination)
    }
    /// Waits for authenticated plaintext or orderly TLS EOF.
    pub fn read(&mut self, destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        self.inner.read(destination)
    }
    /// Admits plaintext to the bounded TLS send queue without blocking.
    pub fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.inner.try_send(bytes)
    }
    /// Waits for enough send credit to admit the complete plaintext buffer.
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.inner.send(bytes)
    }
    /// Requests an orderly TLS `close_notify` after accepted writes drain.
    pub fn try_finish(&mut self) -> Result<TcpFinishStatus, TcpFinishError> {
        self.inner.try_finish()
    }
    /// Waits until the orderly TLS write close completes.
    pub fn finish(&mut self) -> Result<(), TcpFinishError> {
        self.inner.finish()
    }
    /// Registers a callback for the orderly TLS write close.
    pub fn finish_with<F>(&mut self, callback: F) -> Result<(), Error>
    where
        F: FnOnce(Result<(), TcpFinishError>) + Send + 'static,
    {
        self.inner.finish_with(callback)
    }
}

/// Read half of a verified standalone TLS stream.
pub struct TlsReader {
    inner: TcpReader,
    _not_sync: PhantomData<Cell<()>>,
}
impl fmt::Debug for TlsReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsReader").finish_non_exhaustive()
    }
}
impl TlsReader {
    /// Returns the connection's cancellation handle and request identity.
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        self.inner.handle()
    }
    /// Reads available authenticated plaintext without blocking.
    pub fn try_read(&mut self, destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        self.inner.try_read(destination)
    }
    /// Waits for authenticated plaintext or orderly TLS EOF.
    pub fn read(&mut self, destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        self.inner.read(destination)
    }
}

/// Write half of a verified standalone TLS stream.
pub struct TlsWriter {
    inner: TcpWriter,
    _not_sync: PhantomData<Cell<()>>,
}
impl fmt::Debug for TlsWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsWriter").finish_non_exhaustive()
    }
}
impl TlsWriter {
    /// Returns the connection's cancellation handle and request identity.
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        self.inner.handle()
    }
    /// Admits plaintext to the bounded TLS send queue without blocking.
    pub fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.inner.try_send(bytes)
    }
    /// Waits for enough send credit to admit the complete plaintext buffer.
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.inner.send(bytes)
    }
    /// Requests an orderly TLS `close_notify` after accepted writes drain.
    pub fn try_finish(&mut self) -> Result<TcpFinishStatus, TcpFinishError> {
        self.inner.try_finish()
    }
    /// Waits until the orderly TLS write close completes.
    pub fn finish(&mut self) -> Result<(), TcpFinishError> {
        self.inner.finish()
    }
    /// Registers a callback for the orderly TLS write close.
    pub fn finish_with<F>(&mut self, callback: F) -> Result<(), Error>
    where
        F: FnOnce(Result<(), TcpFinishError>) + Send + 'static,
    {
        self.inner.finish_with(callback)
    }
}

impl TcpConnector {
    /// Starts a verified TLS connection and delivers one terminal callback.
    pub fn start_tls<F>(
        &self,
        request: TcpConnectRequest,
        options: TlsOptions,
        callback: F,
    ) -> Result<TlsConnectHandle, Error>
    where
        F: FnOnce(TlsConnectCompletion) + Send + 'static,
    {
        let callback: TlsConnectCallback = Box::new(callback);
        let accepted =
            self.shared
                .accept_tls_connect(self.clone(), request, options, Some(callback))?;
        Ok(TlsConnectHandle::new(self.clone(), accepted.id()))
    }

    /// Submits a verified TLS connection and returns a passive waiter.
    pub fn submit_tls(
        &self,
        request: TcpConnectRequest,
        options: TlsOptions,
    ) -> Result<PendingTlsConnect, Error> {
        let state = self
            .shared
            .accept_tls_connect(self.clone(), request, options, None)?;
        Ok(PendingTlsConnect::new(
            TlsConnectHandle::new(self.clone(), state.id()),
            state,
        ))
    }

    /// Establishes TLS and waits for verification on a spawned Engine.
    pub fn execute_tls(
        &self,
        request: TcpConnectRequest,
        options: TlsOptions,
    ) -> Result<TlsConnection, ExecuteError> {
        if self.shared.run_mode == RunMode::Manual {
            return Err(ExecuteError::Submission(Error::new(
                ErrorKind::WrongMode,
                "blocking TLS connect requires a spawned Engine",
            )));
        }
        match self.submit_tls(request, options) {
            Ok(pending) => match pending.wait() {
                TlsConnectCompletion::Completed(connection) => Ok(connection),
                TlsConnectCompletion::Failed(error) => Err(ExecuteError::Failed(error)),
                TlsConnectCompletion::Cancelled => Err(ExecuteError::Cancelled),
            },
            Err(error) => Err(ExecuteError::Submission(error)),
        }
    }
}

impl TcpConnection {
    /// Consumes an idle unsplit TCP connection and starts verified TLS upgrade.
    pub fn start_tls<F>(
        mut self,
        options: TlsOptions,
        callback: F,
    ) -> Result<TlsConnectHandle, Error>
    where
        F: FnOnce(TlsConnectCompletion) + Send + 'static,
    {
        let callback: TlsConnectCallback = Box::new(callback);
        let state = self.handle.connector.shared.accept_tls_upgrade(
            self.handle.connector.clone(),
            Arc::clone(&self.io),
            options,
            Some(callback),
        )?;
        self.live = false;
        Ok(TlsConnectHandle::new(
            self.handle.connector.clone(),
            state.id(),
        ))
    }

    /// Consumes an idle unsplit TCP connection and returns an upgrade waiter.
    pub fn submit_tls(mut self, options: TlsOptions) -> Result<PendingTlsConnect, Error> {
        let state = self.handle.connector.shared.accept_tls_upgrade(
            self.handle.connector.clone(),
            Arc::clone(&self.io),
            options,
            None,
        )?;
        self.live = false;
        Ok(PendingTlsConnect::new(
            TlsConnectHandle::new(self.handle.connector.clone(), state.id()),
            state,
        ))
    }

    /// Consumes an idle unsplit TCP connection and waits for verified upgrade.
    pub fn into_tls(self, options: TlsOptions) -> Result<TlsConnection, ExecuteError> {
        if self.handle.connector.shared.run_mode == RunMode::Manual {
            return Err(ExecuteError::Submission(Error::new(
                ErrorKind::WrongMode,
                "blocking TLS upgrade requires a spawned Engine",
            )));
        }
        match self.submit_tls(options) {
            Ok(pending) => match pending.wait() {
                TlsConnectCompletion::Completed(connection) => Ok(connection),
                TlsConnectCompletion::Failed(error) => Err(ExecuteError::Failed(error)),
                TlsConnectCompletion::Cancelled => Err(ExecuteError::Cancelled),
            },
            Err(error) => Err(ExecuteError::Submission(error)),
        }
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;

    #[test]
    fn tls_identity_admission_rejects_invalid_numeric_tlds_and_accepts_ip() {
        for invalid in ["mail.123", "127.0.0.999", "bad..example"] {
            assert_eq!(
                TlsOptions::new(invalid)
                    .expect_err("invalid TLS identity")
                    .kind(),
                ErrorKind::InvalidRequest,
                "{invalid} must fail before admission",
            );
        }
        assert_eq!(
            TlsOptions::new("127.0.0.1")
                .expect("IPv4 identity")
                .server_name(),
            "127.0.0.1"
        );
        assert_eq!(
            TlsOptions::new("::1").expect("IPv6 identity").server_name(),
            "::1"
        );
        assert_eq!(
            TlsOptions::new("mail.example")
                .expect("DNS identity")
                .server_name(),
            "mail.example"
        );
    }

    #[test]
    fn tls_establishment_deadline_must_be_finite_and_representable() {
        let options = TlsOptions::new("mail.example").expect("DNS identity");
        assert_eq!(
            options
                .clone()
                .handshake_timeout(Duration::ZERO)
                .validate()
                .expect_err("zero timeout rejected")
                .kind(),
            ErrorKind::InvalidRequest,
        );
        assert_eq!(
            options
                .handshake_timeout(Duration::MAX)
                .validate()
                .expect_err("unrepresentable timeout rejected")
                .kind(),
            ErrorKind::InvalidRequest,
        );
    }
}
