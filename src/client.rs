use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

use crate::engine::Shared;
use crate::{Completion, Error, ErrorKind, ExecuteError, Request, RequestId, Response};

/// Cheap cloneable command handle issued by an [`Engine`](crate::Engine).
///
/// Client has deliberately no public constructor. It does not own or extend Engine lifetime.
#[derive(Clone, Debug)]
pub struct Client {
    pub(crate) shared: Arc<Shared>,
}

impl Client {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// Starts a callback-oriented request.
    ///
    /// Callback storage is `Send` by construction and will be dispatched only from an owned event
    /// after terminal state is committed. The scaffold backend rejects requests before acceptance.
    pub fn start<F>(&self, request: Request, callback: F) -> Result<RequestHandle, Error>
    where
        F: FnOnce(Completion) + Send + 'static,
    {
        self.ensure_running()?;
        drop(request);
        drop(callback);
        Err(backend_not_implemented())
    }

    /// Submits a request and returns its direct terminal-state waiter.
    pub fn submit(&self, request: Request) -> Result<PendingRequest, Error> {
        self.ensure_running()?;
        drop(request);
        Err(backend_not_implemented())
    }

    /// Submits a request and blocks on its direct terminal-state waiter.
    pub fn execute(&self, request: Request) -> Result<Response, ExecuteError> {
        let pending = self.submit(request).map_err(ExecuteError::Submission)?;
        match pending.wait() {
            Completion::Completed(response) => Ok(response),
            Completion::Failed(error) => Err(ExecuteError::Failed(error)),
            Completion::Cancelled => Err(ExecuteError::Cancelled),
        }
    }

    /// Cancels an Engine-scoped request ID.
    ///
    /// Cancellation is idempotent for same-Engine terminal requests, including after Engine stop.
    /// An ID issued by another Engine fails closed.
    pub fn cancel(&self, request_id: RequestId) -> Result<(), Error> {
        if request_id.engine != self.shared.id {
            return Err(Error::new(
                ErrorKind::WrongEngine,
                "request ID belongs to another Engine",
            ));
        }

        // WP1 installs the request registry and wakeup command. With no accepted requests in WP0,
        // every representable same-Engine ID is necessarily terminal and cancellation is a no-op.
        Ok(())
    }

    fn ensure_running(&self) -> Result<(), Error> {
        if self.shared.stopped.load(Ordering::Acquire) {
            Err(Error::new(
                ErrorKind::EngineStopped,
                "the owning Engine has stopped",
            ))
        } else {
            Ok(())
        }
    }
}

/// Engine-bound control handle for one accepted request.
#[derive(Clone, Debug)]
pub struct RequestHandle {
    client: Client,
    id: RequestId,
}

impl RequestHandle {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn new(client: Client, id: RequestId) -> Self {
        Self { client, id }
    }

    /// Returns the opaque Engine-scoped request identity.
    #[must_use]
    pub fn id(&self) -> RequestId {
        self.id
    }

    /// Requests cancellation. Repeated and post-terminal calls are harmless.
    pub fn cancel(&self) -> Result<(), Error> {
        self.client.cancel(self.id)
    }

    /// Converts this handle into an opt-in cancel-on-drop guard.
    #[must_use]
    pub fn cancel_on_drop(self) -> CancelOnDrop {
        CancelOnDrop { handle: Some(self) }
    }
}

/// Opt-in guard that requests cancellation when dropped.
#[derive(Debug)]
pub struct CancelOnDrop {
    handle: Option<RequestHandle>,
}

impl CancelOnDrop {
    /// Cancels immediately and consumes the guard.
    pub fn cancel(mut self) -> Result<(), Error> {
        match self.handle.take() {
            Some(handle) => handle.cancel(),
            None => Ok(()),
        }
    }

    /// Disarms the guard and returns the ordinary non-cancelling handle.
    #[must_use]
    pub fn disarm(mut self) -> RequestHandle {
        match self.handle.take() {
            Some(handle) => handle,
            None => unreachable!("CancelOnDrop always owns a handle until consumed"),
        }
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ignored = handle.cancel();
        }
    }
}

/// Accepted request plus a direct terminal-state waiter.
#[derive(Debug)]
pub struct PendingRequest {
    handle: RequestHandle,
    completion: Receiver<Completion>,
}

impl PendingRequest {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn completed(handle: RequestHandle, completion: Completion) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ignored = sender.send(completion);
        Self {
            handle,
            completion: receiver,
        }
    }

    /// Returns a clone of the independent cancellation handle.
    #[must_use]
    pub fn handle(&self) -> RequestHandle {
        self.handle.clone()
    }

    /// Waits for and returns the canonical terminal outcome.
    #[must_use]
    pub fn wait(self) -> Completion {
        match self.completion.recv() {
            Ok(completion) => completion,
            Err(_disconnected) => Completion::Failed(Error::new(
                ErrorKind::Internal,
                "request completion channel closed before terminal state",
            )),
        }
    }

    /// Waits locally without changing request state or cancelling on timeout.
    #[must_use]
    pub fn wait_for(self, duration: Duration) -> WaitOutcome {
        match self.completion.recv_timeout(duration) {
            Ok(completion) => WaitOutcome::Completed(completion),
            Err(RecvTimeoutError::Timeout) => WaitOutcome::TimedOut(self),
            Err(RecvTimeoutError::Disconnected) => {
                WaitOutcome::Completed(Completion::Failed(Error::new(
                    ErrorKind::Internal,
                    "request completion channel closed before terminal state",
                )))
            }
        }
    }
}

/// Result of a waiter-local timeout.
#[derive(Debug)]
#[non_exhaustive]
pub enum WaitOutcome {
    /// The request reached its canonical terminal outcome.
    Completed(Completion),
    /// The local wait expired; the request and its cancellation handle remain live.
    TimedOut(PendingRequest),
}

fn backend_not_implemented() -> Error {
    Error::new(
        ErrorKind::BackendUnavailable,
        "no production HTTP backend is implemented in WP0",
    )
}
