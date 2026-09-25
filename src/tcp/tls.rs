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

    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

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
        Ok(())
    }
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

    #[must_use]
    pub fn id(&self) -> RequestId {
        self.id
    }

    pub fn cancel(&self) -> Result<(), Error> {
        self.connector.cancel(self.id)
    }
}

/// Canonical outcome of either an immediate TLS connect or a consuming upgrade.
#[derive(Debug)]
#[non_exhaustive]
pub enum TlsConnectCompletion {
    Completed(TlsConnection),
    Failed(Error),
    Cancelled,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum TlsConnectWaitOutcome {
    Completed(TlsConnectCompletion),
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

    #[must_use]
    pub fn handle(&self) -> TlsConnectHandle {
        self.handle.clone()
    }

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

    #[must_use]
    pub fn wait(self) -> TlsConnectCompletion {
        self.state.wait()
    }

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
    pub(crate) fn from_tcp(inner: TcpConnection) -> Self {
        Self {
            inner,
            _not_sync: PhantomData,
        }
    }

    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        self.inner.handle()
    }
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.inner.local_addr()
    }
    pub fn peer_addr(&self) -> Result<SocketAddr, Error> {
        self.inner.peer_addr()
    }
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
    pub fn try_read(&mut self, destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        self.inner.try_read(destination)
    }
    pub fn read(&mut self, destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        self.inner.read(destination)
    }
    pub fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.inner.try_send(bytes)
    }
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.inner.send(bytes)
    }
    pub fn try_finish(&mut self) -> Result<TcpFinishStatus, TcpFinishError> {
        self.inner.try_finish()
    }
    pub fn finish(&mut self) -> Result<(), TcpFinishError> {
        self.inner.finish()
    }
    pub fn finish_with<F>(&mut self, callback: F) -> Result<(), Error>
    where
        F: FnOnce(Result<(), TcpFinishError>) + Send + 'static,
    {
        self.inner.finish_with(callback)
    }
}

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
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        self.inner.handle()
    }
    pub fn try_read(&mut self, destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        self.inner.try_read(destination)
    }
    pub fn read(&mut self, destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        self.inner.read(destination)
    }
}

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
    #[must_use]
    pub fn handle(&self) -> TcpConnectionHandle {
        self.inner.handle()
    }
    pub fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.inner.try_send(bytes)
    }
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.inner.send(bytes)
    }
    pub fn try_finish(&mut self) -> Result<TcpFinishStatus, TcpFinishError> {
        self.inner.try_finish()
    }
    pub fn finish(&mut self) -> Result<(), TcpFinishError> {
        self.inner.finish()
    }
    pub fn finish_with<F>(&mut self, callback: F) -> Result<(), Error>
    where
        F: FnOnce(Result<(), TcpFinishError>) + Send + 'static,
    {
        self.inner.finish_with(callback)
    }
}

impl TcpConnector {
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
