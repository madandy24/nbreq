use std::sync::Arc;
use std::time::Duration;

use crate::registry::{CompletionCallback, RequestState, Shared};
use crate::{Completion, Error, ExecuteError, Request, RequestId, Response};

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
    /// The callback is queued only after canonical terminal state is committed. It never executes
    /// on the network reactor or while the request registry is locked.
    pub fn start<F>(&self, request: Request, callback: F) -> Result<RequestHandle, Error>
    where
        F: FnOnce(Completion) + Send + 'static,
    {
        let callback: CompletionCallback = Box::new(callback);
        let accepted = self.shared.accept(request, Some(callback))?;
        Ok(RequestHandle::new(self.clone(), accepted.state.id()))
    }

    /// Submits a request and returns its direct terminal-state waiter.
    pub fn submit(&self, request: Request) -> Result<PendingRequest, Error> {
        let accepted = self.shared.accept(request, None)?;
        let handle = RequestHandle::new(self.clone(), accepted.state.id());
        Ok(PendingRequest {
            handle,
            state: accepted.state,
        })
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
        self.shared.cancel(request_id)
    }
}

/// Engine-bound control handle for one accepted request.
#[derive(Clone, Debug)]
pub struct RequestHandle {
    client: Client,
    id: RequestId,
}

impl RequestHandle {
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
    state: Arc<RequestState>,
}

impl PendingRequest {
    /// Returns a clone of the independent cancellation handle.
    #[must_use]
    pub fn handle(&self) -> RequestHandle {
        self.handle.clone()
    }

    /// Returns whether canonical terminal state has been committed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state.is_terminal()
    }

    pub(crate) fn try_completion(&self) -> Option<Completion> {
        self.state.completion()
    }

    pub(crate) fn request_id(&self) -> RequestId {
        self.state.id()
    }

    /// Waits for and returns the canonical terminal outcome.
    #[must_use]
    pub fn wait(self) -> Completion {
        self.state.wait()
    }

    /// Waits locally without changing request state or cancelling on timeout.
    #[must_use]
    pub fn wait_for(self, duration: Duration) -> WaitOutcome {
        match self.state.wait_for(duration) {
            Some(completion) => WaitOutcome::Completed(completion),
            None => WaitOutcome::TimedOut(self),
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

#[cfg(test)]
mod tests {
    use crate::testing;
    use crate::{EngineConfig, ErrorKind};

    use super::*;

    fn request() -> Request {
        Request::get("https://example.invalid/")
            .build()
            .expect("test request must build")
    }

    #[test]
    fn late_cancel_is_idempotent_but_wrong_engine_fails_closed() {
        let (first, controller) = testing::engine(EngineConfig::spawned())
            .expect("first deterministic Engine must construct");
        let first_client = first.client();
        let pending = first_client.submit(request()).expect("request must submit");
        let handle = pending.handle();
        assert!(controller.complete(handle.id(), Completion::Cancelled));

        let second =
            crate::Engine::new(EngineConfig::spawned()).expect("second Engine must construct");
        let second_client = second.client();
        let error = second_client
            .cancel(handle.id())
            .expect_err("cross-Engine cancellation must fail closed");
        assert_eq!(error.kind(), ErrorKind::WrongEngine);

        first.shutdown().expect("first Engine must stop");
        handle
            .cancel()
            .expect("same-Engine cancellation remains harmless after stop");
        second.shutdown().expect("second Engine must stop");
    }
}
