use std::sync::Arc;
use std::time::Duration;

use crate::registry::{CompletionCallback, RequestState, Shared};
use crate::{
    Completion, Error, ErrorKind, ExecuteError, Request, RequestBuilder, RequestId, RequestOptions,
    Response, ResponseReader, RunMode, StreamRequest, TlsVerification,
};

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

    /// Submits a request whose response is consumed through one unique reader.
    ///
    /// Streaming is available on the native backend. Internal non-networking test backends return
    /// `Unsupported` without accepting the request.
    pub fn submit_stream(&self, request: StreamRequest) -> Result<ResponseReader, Error> {
        self.shared.accept_stream(request)
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

/// Engine-bound builder for a simple buffered HTTP request.
///
/// This builder owns a cheap clone of the Engine's [`Client`] ticket and delegates to the same
/// [`RequestBuilder`] and [`Client::execute`] path as the explicit API. It does not own or extend
/// the Engine lifetime, create another Engine, or select a different backend.
#[derive(Clone, Debug)]
pub struct EngineRequestBuilder {
    client: Client,
    request: RequestBuilder,
}

impl EngineRequestBuilder {
    pub(crate) fn new(client: Client, request: RequestBuilder) -> Self {
        Self { client, request }
    }

    /// Adds an owned header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.request = self.request.header(name, value);
        self
    }

    /// Sets the buffered request body.
    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.request = self.request.body(body);
        self
    }

    /// Sets all portable request options.
    #[must_use]
    pub fn options(mut self, options: RequestOptions) -> Self {
        self.request = self.request.options(options);
        self
    }

    /// Sets the maximum connection-establishment duration.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.request = self.request.connect_timeout(timeout);
        self
    }

    /// Sets the maximum duration without useful I/O progress.
    #[must_use]
    pub fn inactivity_timeout(mut self, timeout: Duration) -> Self {
        self.request = self.request.inactivity_timeout(timeout);
        self
    }

    /// Sets the maximum total duration beginning at request acceptance.
    #[must_use]
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        self.request = self.request.total_timeout(timeout);
        self
    }

    /// Sets the maximum number of redirects followed.
    #[must_use]
    pub fn redirect_limit(mut self, limit: u8) -> Self {
        self.request = self.request.redirect_limit(limit);
        self
    }

    /// Sets HTTPS certificate and hostname verification policy.
    #[must_use]
    pub fn tls_verification(mut self, verification: TlsVerification) -> Self {
        self.request = self.request.tls_verification(verification);
        self
    }

    /// Builds, submits, and waits for the buffered response.
    ///
    /// HTTP error statuses remain ordinary [`Response`] values. A manual Engine returns
    /// [`ErrorKind::WrongMode`] through [`ExecuteError::Submission`] before request admission;
    /// this method never drives an Engine internally.
    pub fn call(self) -> Result<Response, ExecuteError> {
        if self.client.shared.run_mode == RunMode::Manual {
            return Err(ExecuteError::Submission(Error::new(
                ErrorKind::WrongMode,
                "blocking HTTP convenience calls require a spawned Engine",
            )));
        }
        let request = self.request.build().map_err(ExecuteError::Submission)?;
        self.client.execute(request)
    }

    /// Replaces the buffered body, then builds, submits, and waits for the response.
    pub fn send(mut self, body: impl Into<Vec<u8>>) -> Result<Response, ExecuteError> {
        self.request = self.request.body(body);
        self.call()
    }

    /// Replaces the buffered body with an empty body, then executes the request.
    pub fn send_empty(mut self) -> Result<Response, ExecuteError> {
        self.request = self.request.body(Vec::new());
        self.call()
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
    ///
    /// Cancellation stops local processing promptly but cannot undo effects already accepted by a
    /// remote server.
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
    use crate::{EngineConfig, ErrorKind, Method, TlsVerification};

    use super::*;

    fn request() -> Request {
        Request::get("https://example.invalid/")
            .build()
            .expect("test request must build")
    }

    #[test]
    fn engine_request_builder_forwards_existing_request_controls() {
        let (engine, _controller) =
            testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
        let request = engine
            .post("http://example.test/items")
            .header("X-Test", b"value".to_vec())
            .body(b"original".to_vec())
            .connect_timeout(Duration::from_millis(101))
            .inactivity_timeout(Duration::from_millis(202))
            .total_timeout(Duration::from_millis(303))
            .redirect_limit(4)
            .tls_verification(TlsVerification::DangerouslyDisableCertificateVerification)
            .request
            .build()
            .expect("forwarded request must build");

        assert_eq!(request.method(), &Method::Post);
        assert_eq!(request.url(), "http://example.test/items");
        assert_eq!(request.headers().len(), 1);
        assert_eq!(request.headers()[0].name(), "X-Test");
        assert_eq!(request.headers()[0].value(), b"value");
        assert_eq!(request.body(), b"original");
        assert_eq!(
            request.options().connect_timeout,
            Some(Duration::from_millis(101))
        );
        assert_eq!(
            request.options().inactivity_timeout,
            Some(Duration::from_millis(202))
        );
        assert_eq!(
            request.options().total_timeout,
            Some(Duration::from_millis(303))
        );
        assert_eq!(request.options().redirect_limit, 4);
        assert_eq!(
            request.options().tls_verification,
            TlsVerification::DangerouslyDisableCertificateVerification
        );
        engine.shutdown().expect("deterministic Engine must stop");
    }

    #[test]
    fn engine_request_builder_rejects_manual_mode_before_admission() {
        let (engine, controller) = testing::engine(EngineConfig::manual())
            .expect("manual deterministic Engine must construct");
        let before = engine.metrics();
        let error = engine
            .get("http://example.test/")
            .call()
            .expect_err("blocking convenience must reject manual mode");
        assert!(matches!(
            error,
            ExecuteError::Submission(ref error) if error.kind() == ErrorKind::WrongMode
        ));
        assert_eq!(controller.active_requests(), 0);
        assert_eq!(engine.metrics(), before);
        engine.shutdown().expect("manual Engine must stop");
    }

    #[test]
    fn engine_request_builder_ticket_does_not_extend_engine_lifetime() {
        let (engine, _controller) =
            testing::engine(EngineConfig::spawned()).expect("deterministic Engine must construct");
        let request = engine.get("http://example.test/");
        engine.shutdown().expect("Engine must stop");

        let error = request
            .call()
            .expect_err("ticket must observe the stopped Engine");
        assert!(matches!(
            error,
            ExecuteError::Submission(ref error) if error.kind() == ErrorKind::EngineStopped
        ));
    }

    #[test]
    fn late_cancel_is_idempotent_but_wrong_engine_fails_closed() {
        let (first, controller) = testing::engine(EngineConfig::spawned())
            .expect("first deterministic Engine must construct");
        let first_client = first.client();
        let pending = first_client.submit(request()).expect("request must submit");
        let handle = pending.handle();
        assert!(controller.complete(handle.id(), Completion::Cancelled));

        let (second, _second_controller) = testing::engine(EngineConfig::spawned())
            .expect("second deterministic Engine must construct");
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

    #[test]
    fn distinct_stream_submission_is_cancelled_without_a_completion_adapter() {
        let (engine, controller) = testing::engine(EngineConfig::spawned())
            .expect("stream-capable held Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                crate::StreamRequest::get("http://example.test/")
                    .build()
                    .expect("stream request must build"),
            )
            .expect("stream request must be accepted");
        assert_eq!(controller.active_requests(), 1);
        reader
            .handle()
            .cancel()
            .expect("stream cancel must succeed");
        assert!(matches!(
            reader.try_head(),
            Err(crate::StreamError::Cancelled)
        ));
        assert_eq!(controller.active_requests(), 0);
        engine.shutdown().expect("held Engine must stop");
    }

    #[test]
    fn aggregate_stream_queue_reservations_are_strict_and_release_on_cancel() {
        let config = EngineConfig::spawned()
            .with_max_stream_queue_bytes_per_request(8)
            .with_max_stream_queued_bytes(8);
        let (engine, _controller) =
            testing::engine(config).expect("bounded held Engine must construct");
        let client = engine.client();
        let first = client
            .submit_stream(
                crate::StreamRequest::get("http://example.test/one")
                    .build()
                    .expect("first stream request must build"),
            )
            .expect("one response window must fit");
        assert_eq!(engine.metrics().current().reserved_stream_queue_bytes(), 8);
        assert_eq!(
            engine.metrics().high_water().reserved_stream_queue_bytes(),
            8
        );
        let error = client
            .submit_stream(
                crate::StreamRequest::get("http://example.test/two")
                    .build()
                    .expect("second stream request must build"),
            )
            .expect_err("a second reserved response window must exceed the budget");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(
            error.limit_kind(),
            Some(crate::LimitKind::StreamingQueueBytes)
        );
        assert_eq!(engine.metrics().requests_accepted(), 1);
        first.handle().cancel().expect("first stream must cancel");
        assert_eq!(engine.metrics().current().reserved_stream_queue_bytes(), 0);
        assert_eq!(engine.metrics().requests_cancelled(), 1);
        let second = client
            .submit_stream(
                crate::StreamRequest::get("http://example.test/two")
                    .build()
                    .expect("second stream request must rebuild"),
            )
            .expect("cancel must release the aggregate reservation");
        assert_eq!(engine.metrics().requests_accepted(), 2);
        second.handle().cancel().expect("second stream must cancel");
        engine.shutdown().expect("held Engine must stop");
    }

    #[test]
    fn non_streaming_backend_rejects_before_acceptance() {
        let engine =
            crate::Engine::with_backend(EngineConfig::manual(), crate::backend::scaffold())
                .expect("scaffold Engine must construct");
        let error = engine
            .client()
            .submit_stream(
                crate::StreamRequest::get("http://example.test/")
                    .build()
                    .expect("stream request must build"),
            )
            .expect_err("scaffold must reject streaming");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(engine.shared_for_testing().active_count(), 0);
        engine.shutdown().expect("scaffold Engine must stop");
    }
}
