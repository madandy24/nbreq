use std::cell::Cell;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::backend::{self, Backend};
use crate::context::{ContextGuard, ContextKind};
use crate::dispatch::DispatcherOwner;
use crate::metrics::Metrics;
use crate::reactor::spawned_main_factory;
use crate::reactor::{ReactorCore, reactor_panicked, spawned_main};
use crate::registry::Shared;
use crate::{
    Client, Completion, DriveStatus, EngineConfig, Error, ErrorKind, PendingRequest, RunMode,
    ShutdownError, ShutdownOutcome,
};

static NEXT_ENGINE_ID: AtomicU64 = AtomicU64::new(1);

enum RuntimeOwner {
    Spawned(Option<JoinHandle<Result<(), ShutdownError>>>),
    #[cfg_attr(feature = "curl-pilot", allow(dead_code))]
    Manual(Option<ReactorCore<dyn Backend + Send>>),
    Stopped,
}

/// Unique owner of one lifecycle, resource, and bulk-cancellation domain.
///
/// Engine is `Send`, deliberately non-cloneable, and deliberately not `Sync` in the initial
/// contract. Clients are obtained only through [`Engine::client`]. Explicit shutdown consumes the
/// Engine owner.
///
/// ```compile_fail
/// use nbreq::Engine;
/// fn require_sync<T: Sync>() {}
/// require_sync::<Engine>();
/// ```
pub struct Engine {
    shared: Arc<Shared>,
    config: EngineConfig,
    runtime: RuntimeOwner,
    dispatcher: Option<DispatcherOwner>,
    shutdown_failure: Option<Error>,
    driving: bool,
    _not_sync: PhantomData<Cell<()>>,
}

struct DrivingGuard<'flag> {
    driving: &'flag mut bool,
}

impl Drop for DrivingGuard<'_> {
    fn drop(&mut self) {
        *self.driving = false;
    }
}

impl Engine {
    /// Creates one independent Engine from backend-neutral configuration.
    ///
    /// With the `curl-pilot` feature, the Engine uses the private spawned curl backend. Without a
    /// transport feature it uses the non-networking scaffold.
    pub fn new(config: EngineConfig) -> Result<Self, Error> {
        #[cfg(feature = "curl-pilot")]
        {
            Self::with_curl_backend(config)
        }
        #[cfg(not(feature = "curl-pilot"))]
        {
            Self::with_backend(config, backend::scaffold())
        }
    }

    #[cfg_attr(feature = "curl-pilot", allow(dead_code))]
    pub(crate) fn with_backend(
        config: EngineConfig,
        mut backend: Box<dyn Backend + Send>,
    ) -> Result<Self, Error> {
        let streaming_supported = backend.supports_streaming();
        let id = NEXT_ENGINE_ID.fetch_add(1, Ordering::Relaxed);
        if id == u64::MAX {
            return Err(Error::new(
                ErrorKind::Internal,
                "Engine identity space is exhausted",
            ));
        }

        let metrics = Arc::new(Metrics::default());
        let dispatcher = DispatcherOwner::new(
            id,
            config.callback_queue_capacity().get(),
            config.callback_dispatch(),
            Arc::clone(&metrics),
        )?;
        let shared = Shared::new(
            id,
            &config,
            dispatcher.domain(),
            streaming_supported,
            Arc::clone(&metrics),
        );
        backend.attach_metrics(metrics);
        let runtime = match config.run_mode() {
            RunMode::Spawned => {
                let reactor_shared = Arc::clone(&shared);
                let reactor = ReactorCore::new(backend);
                let handle = thread::Builder::new()
                    .name(format!("nbreq-reactor-{id}"))
                    .spawn(move || spawned_main(reactor_shared, reactor))
                    .map_err(|error| {
                        shared.begin_shutdown();
                        dispatcher.seal();
                        Error::new(
                            ErrorKind::Internal,
                            format!("failed to spawn NBReq reactor: {error}"),
                        )
                    });
                let handle = match handle {
                    Ok(handle) => handle,
                    Err(error) => {
                        let _callback_result = dispatcher.finish();
                        return Err(error);
                    }
                };
                RuntimeOwner::Spawned(Some(handle))
            }
            RunMode::Manual => RuntimeOwner::Manual(Some(ReactorCore::new(backend))),
        };

        Ok(Self {
            shared,
            config,
            runtime,
            dispatcher: Some(dispatcher),
            shutdown_failure: None,
            driving: false,
            _not_sync: PhantomData,
        })
    }

    #[cfg(feature = "curl-pilot")]
    pub(crate) fn with_curl_backend(config: EngineConfig) -> Result<Self, Error> {
        let factory = backend::curl_factory(&config);
        Self::with_curl_factory(config, factory)
    }

    #[cfg(all(test, feature = "curl-pilot"))]
    pub(crate) fn with_curl_test_ca(config: EngineConfig, ca_pem: Vec<u8>) -> Result<Self, Error> {
        let factory = backend::curl_factory_with_test_ca(&config, ca_pem);
        Self::with_curl_factory(config, factory)
    }

    #[cfg(feature = "curl-pilot")]
    fn with_curl_factory(
        config: EngineConfig,
        factory: Box<dyn backend::BackendFactory>,
    ) -> Result<Self, Error> {
        if config.run_mode() != RunMode::Spawned {
            return Err(Error::new(
                ErrorKind::WrongMode,
                "the private curl proving backend supports spawned mode only",
            ));
        }

        Self::with_spawned_factory(config, factory)
    }

    #[cfg_attr(not(feature = "curl-pilot"), allow(dead_code))]
    pub(crate) fn with_spawned_factory(
        config: EngineConfig,
        factory: Box<dyn backend::BackendFactory>,
    ) -> Result<Self, Error> {
        if config.run_mode() != RunMode::Spawned {
            return Err(Error::new(
                ErrorKind::WrongMode,
                "a spawned backend factory requires spawned Engine mode",
            ));
        }
        let id = NEXT_ENGINE_ID.fetch_add(1, Ordering::Relaxed);
        if id == u64::MAX {
            return Err(Error::new(
                ErrorKind::Internal,
                "Engine identity space is exhausted",
            ));
        }
        let metrics = Arc::new(Metrics::default());
        let dispatcher = DispatcherOwner::new(
            id,
            config.callback_queue_capacity().get(),
            config.callback_dispatch(),
            Arc::clone(&metrics),
        )?;
        let streaming_supported = factory.supports_streaming();
        let shared = Shared::new(
            id,
            &config,
            dispatcher.domain(),
            streaming_supported,
            metrics,
        );
        let reactor_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name(format!("nbreq-reactor-{id}"))
            .spawn(move || spawned_main_factory(reactor_shared, factory))
            .map_err(|error| {
                shared.begin_shutdown();
                dispatcher.seal();
                Error::new(
                    ErrorKind::Internal,
                    format!("failed to spawn NBReq reactor factory: {error}"),
                )
            });
        let handle = match handle {
            Ok(handle) => handle,
            Err(error) => {
                let _callback_result = dispatcher.finish();
                return Err(error);
            }
        };

        Ok(Self {
            shared,
            config,
            runtime: RuntimeOwner::Spawned(Some(handle)),
            dispatcher: Some(dispatcher),
            shutdown_failure: None,
            driving: false,
            _not_sync: PhantomData,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn shared_for_testing(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    /// Starts a configuration builder in the convenient spawned mode.
    #[must_use]
    pub fn builder() -> EngineBuilder {
        EngineBuilder::spawned()
    }

    /// Issues a cheap cloneable command handle for this Engine.
    #[must_use]
    pub fn client(&self) -> Client {
        Client::new(Arc::clone(&self.shared))
    }

    /// Returns a nonblocking, payload-free snapshot of this Engine's lifetime activity.
    ///
    /// Fields are loaded independently and may be cross-field inconsistent while work is moving.
    #[must_use]
    pub fn metrics(&self) -> crate::EngineMetrics {
        self.shared.metrics_snapshot()
    }

    /// Cancels requests accepted before the cancellation barrier while keeping the Engine alive.
    pub fn cancel_all(&self) {
        self.shared.cancel_all();
    }

    /// Performs one host-driven Engine pass up to `deadline`.
    pub fn drive(&mut self, deadline: Instant) -> Result<DriveStatus, Error> {
        if self.config.run_mode() != RunMode::Manual {
            return Err(Error::new(
                ErrorKind::WrongMode,
                "drive is available only for a manual Engine",
            ));
        }
        if self.driving {
            return Err(Error::new(
                ErrorKind::ReentrantDrive,
                "manual Engine drive is already active",
            ));
        }

        let (driving, runtime, dispatcher) =
            (&mut self.driving, &mut self.runtime, &self.dispatcher);
        *driving = true;
        let _driving = DrivingGuard { driving };
        let _context = ContextGuard::enter(self.shared.id, ContextKind::Drive);
        let result = match runtime {
            RuntimeOwner::Manual(Some(reactor)) => {
                let seen_generation = self.shared.queue.generation();
                let mut result = reactor.drive(&self.shared, deadline);
                if matches!(result, Ok(DriveStatus::Idle))
                    && reactor.transport_wait().is_none()
                    && Instant::now() < deadline
                {
                    if self.shared.queue.wait_for_signal_until(
                        seen_generation,
                        &self.shared.stopped,
                        deadline,
                    ) {
                        result = reactor.drive(&self.shared, deadline);
                    } else {
                        result = Ok(DriveStatus::DeadlineReached);
                    }
                }
                result
            }
            RuntimeOwner::Manual(None) | RuntimeOwner::Stopped => {
                Err(Error::new(ErrorKind::EngineStopped, "Engine has stopped"))
            }
            RuntimeOwner::Spawned(_) => Err(Error::new(
                ErrorKind::WrongMode,
                "drive is available only for a manual Engine",
            )),
        };
        if let Some(dispatcher) = dispatcher {
            dispatcher.drain_inline();
        }
        result
    }

    /// Drives a manual Engine until `pending` reaches its canonical terminal outcome.
    pub fn drive_until(&mut self, pending: PendingRequest) -> Result<Completion, Error> {
        if self.config.run_mode() != RunMode::Manual {
            return Err(Error::new(
                ErrorKind::WrongMode,
                "drive_until is available only for a manual Engine",
            ));
        }
        if pending.request_id().engine != self.shared.id {
            return Err(Error::new(
                ErrorKind::WrongEngine,
                "PendingRequest belongs to another Engine",
            ));
        }
        loop {
            if let Some(completion) = pending.try_completion() {
                return Ok(completion);
            }
            self.drive(Instant::now() + Duration::from_millis(10))?;
        }
    }

    /// Irreversibly stops network work and waits for callback dispatch to drain.
    pub fn shutdown(mut self) -> Result<(), ShutdownError> {
        if crate::context::is_callback(self.shared.id) {
            return match self.defer_cleanup() {
                Ok(()) => Err(reentrant_shutdown_error()),
                Err(error) => Err(error),
            };
        }
        let network = self.stop_network();
        let callbacks = self.finish_callbacks();
        network.and(callbacks)
    }

    /// Irreversibly stops network work, then waits up to `duration` for callback dispatch.
    pub fn shutdown_for(mut self, duration: Duration) -> Result<ShutdownOutcome, ShutdownError> {
        if crate::context::is_callback(self.shared.id) {
            return match self.defer_cleanup() {
                Ok(()) => Err(reentrant_shutdown_error()),
                Err(error) => Err(error),
            };
        }
        if let Err(error) = self.stop_network() {
            let _callback_result = self.finish_callbacks();
            return Err(error);
        }
        let Some(dispatcher) = self.dispatcher.take() else {
            return Ok(ShutdownOutcome::Complete);
        };
        match dispatcher.finish_for(duration)? {
            Some(detached) => Ok(ShutdownOutcome::CallbacksRemaining(detached)),
            None => Ok(ShutdownOutcome::Complete),
        }
    }

    fn stop_network(&mut self) -> Result<(), ShutdownError> {
        self.shared.begin_shutdown();
        if let Some(error) = &self.shutdown_failure {
            if let Some(dispatcher) = &self.dispatcher {
                dispatcher.seal();
            }
            return Err(ShutdownError::new(error.clone()));
        }
        let shutdown = match &mut self.runtime {
            RuntimeOwner::Spawned(handle) => match handle.take() {
                Some(handle) => handle.join().unwrap_or_else(|_| Err(reactor_panicked())),
                None => Ok(()),
            },
            RuntimeOwner::Manual(reactor) => match reactor.as_mut() {
                Some(reactor) => reactor.shutdown(&self.shared),
                None => Ok(()),
            },
            RuntimeOwner::Stopped => Ok(()),
        };

        self.shared.wait_for_callback_activations();
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.seal();
        }
        if let Err(error) = &shutdown {
            if matches!(self.runtime, RuntimeOwner::Spawned(None)) {
                self.shutdown_failure = Some(error.error().clone());
            }
        }
        if shutdown.is_ok() {
            self.runtime = RuntimeOwner::Stopped;
            self.shared.mark_stopped();
        }
        shutdown
    }

    fn finish_callbacks(&mut self) -> Result<(), ShutdownError> {
        match self.dispatcher.take() {
            Some(dispatcher) => dispatcher.finish(),
            None => Ok(()),
        }
    }

    fn defer_cleanup(&mut self) -> Result<(), ShutdownError> {
        self.shared.begin_shutdown();
        let runtime = std::mem::replace(&mut self.runtime, RuntimeOwner::Stopped);
        let dispatcher = self.dispatcher.take();
        let resources = Arc::new(std::sync::Mutex::new(Some((runtime, dispatcher))));
        let cleanup_resources = Arc::clone(&resources);
        let shared = Arc::clone(&self.shared);
        let cleanup = move || {
            let (runtime, dispatcher) = cleanup_resources
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .expect("deferred cleanup resources must be present");
            let network = stop_owned_runtime(runtime, &shared);
            if network.is_ok() {
                shared.mark_stopped();
            }
            shared.wait_for_callback_activations();
            if let Some(dispatcher) = dispatcher {
                let _callback_result = dispatcher.finish();
            }
        };
        match thread::Builder::new()
            .name(format!("nbreq-deferred-shutdown-{}", self.shared.id))
            .spawn(cleanup)
        {
            Ok(_cleanup_thread) => Ok(()),
            Err(error) => {
                let (runtime, dispatcher) = resources
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                    .expect("failed spawn must return deferred cleanup resources");
                self.runtime = runtime;
                self.dispatcher = dispatcher;
                Err(ShutdownError::new(Error::new(
                    ErrorKind::Internal,
                    format!("failed to spawn NBReq deferred cleanup worker: {error}"),
                )))
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if crate::context::is_callback(self.shared.id) {
            if !matches!(self.runtime, RuntimeOwner::Stopped) || self.dispatcher.is_some() {
                let _deferred_result = self.defer_cleanup();
            }
            return;
        }
        let _network_result = self.stop_network();
        let _callback_result = self.finish_callbacks();
    }
}

fn stop_owned_runtime(
    mut runtime: RuntimeOwner,
    shared: &Arc<Shared>,
) -> Result<(), ShutdownError> {
    match &mut runtime {
        RuntimeOwner::Spawned(handle) => match handle.take() {
            Some(handle) => handle.join().unwrap_or_else(|_| Err(reactor_panicked())),
            None => Ok(()),
        },
        RuntimeOwner::Manual(reactor) => match reactor.as_mut() {
            Some(reactor) => reactor.shutdown(shared),
            None => Ok(()),
        },
        RuntimeOwner::Stopped => Ok(()),
    }
}

fn reentrant_shutdown_error() -> ShutdownError {
    ShutdownError::new(Error::new(
        ErrorKind::ReentrantOperation,
        "Engine shutdown cannot synchronously join its own callback stack",
    ))
}

/// Builder for one independently owned Engine.
#[derive(Clone, Debug)]
pub struct EngineBuilder {
    config: EngineConfig,
}

impl EngineBuilder {
    /// Starts a spawned Engine configuration.
    #[must_use]
    pub fn spawned() -> Self {
        Self {
            config: EngineConfig::spawned(),
        }
    }

    /// Starts a manual Engine configuration.
    #[must_use]
    pub fn manual() -> Self {
        Self {
            config: EngineConfig::manual(),
        }
    }

    /// Replaces the backend-neutral configuration.
    #[must_use]
    pub fn config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    /// Selects the Engine-owned callback worker count.
    #[must_use]
    pub fn callback_workers(mut self, workers: NonZeroUsize) -> Self {
        self.config = self.config.with_callback_workers(workers);
        self
    }

    /// Selects the accepted/inflight request bound.
    #[must_use]
    pub fn max_inflight_requests(mut self, requests: NonZeroUsize) -> Self {
        self.config = self.config.with_max_inflight_requests(requests);
        self
    }

    /// Selects the bounded command queue capacity.
    #[must_use]
    pub fn command_queue_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.config = self.config.with_command_queue_capacity(capacity);
        self
    }

    /// Selects the bounded callback event and callback-bearing request capacity.
    #[must_use]
    pub fn callback_queue_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.config = self.config.with_callback_queue_capacity(capacity);
        self
    }

    /// Selects the maximum buffered request-body size.
    #[must_use]
    pub fn max_request_body_bytes(mut self, bytes: usize) -> Self {
        self.config = self.config.with_max_request_body_bytes(bytes);
        self
    }

    /// Selects the maximum buffered response-body size.
    #[must_use]
    pub fn max_response_body_bytes(mut self, bytes: usize) -> Self {
        self.config = self.config.with_max_response_body_bytes(bytes);
        self
    }

    /// Selects the maximum per-request streaming upload or response queue window.
    #[must_use]
    pub fn max_stream_queue_bytes_per_request(mut self, bytes: usize) -> Self {
        self.config = self.config.with_max_stream_queue_bytes_per_request(bytes);
        self
    }

    /// Selects the Engine-wide reserved streaming queue budget.
    #[must_use]
    pub fn max_stream_queued_bytes(mut self, bytes: usize) -> Self {
        self.config = self.config.with_max_stream_queued_bytes(bytes);
        self
    }

    /// Selects the maximum cumulative request/response header bytes.
    #[must_use]
    pub fn max_header_bytes(mut self, bytes: usize) -> Self {
        self.config = self.config.with_max_header_bytes(bytes);
        self
    }

    /// Selects the maximum request/response header field count.
    #[must_use]
    pub fn max_header_count(mut self, count: usize) -> Self {
        self.config = self.config.with_max_header_count(count);
        self
    }

    /// Selects the Engine-wide connecting, leased, and idle connection limit.
    #[must_use]
    pub fn max_connections(mut self, connections: NonZeroUsize) -> Self {
        self.config = self.config.with_max_connections(connections);
        self
    }

    /// Selects the per-origin connecting, leased, and idle connection limit.
    #[must_use]
    pub fn max_connections_per_origin(mut self, connections: NonZeroUsize) -> Self {
        self.config = self.config.with_max_connections_per_origin(connections);
        self
    }

    /// Selects the Engine-wide idle connection limit. Zero disables idle reuse.
    #[must_use]
    pub fn max_idle_connections(mut self, connections: usize) -> Self {
        self.config = self.config.with_max_idle_connections(connections);
        self
    }

    /// Selects the per-origin idle connection limit. Zero disables idle reuse.
    #[must_use]
    pub fn max_idle_connections_per_origin(mut self, connections: usize) -> Self {
        self.config = self
            .config
            .with_max_idle_connections_per_origin(connections);
        self
    }

    /// Selects how long an unused persistent connection may remain pooled.
    #[must_use]
    pub fn idle_connection_timeout(mut self, timeout: Duration) -> Self {
        self.config = self.config.with_idle_connection_timeout(timeout);
        self
    }

    /// Builds one independent Engine.
    pub fn build(self) -> Result<Engine, Error> {
        Engine::new(self.config)
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::backend::BackendCompletion;
    use crate::stream::ResponseSink;
    use crate::{CallbackDispatch, Request, RequestId, StreamError, StreamRequest};

    use super::*;

    struct FailOnceBackend {
        shutdown_attempts: Arc<AtomicUsize>,
    }

    impl Backend for FailOnceBackend {
        fn submit(
            &mut self,
            _id: RequestId,
            _request: Request,
            _accepted_at: Instant,
        ) -> Option<Completion> {
            None
        }

        fn cancel(&mut self, _id: RequestId) {}

        fn poll(&mut self, _deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
            Ok(Vec::new())
        }

        fn shutdown(&mut self) -> Result<(), ShutdownError> {
            if self.shutdown_attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                Err(ShutdownError::new(Error::new(
                    ErrorKind::Internal,
                    "deliberate first shutdown failure",
                )))
            } else {
                Ok(())
            }
        }
    }

    struct PanicOnceBackend {
        should_panic: bool,
    }

    struct PanicOnSubmitBackend;

    struct PanicOnStreamSubmitBackend {
        response: Option<ResponseSink>,
    }

    impl Backend for PanicOnSubmitBackend {
        fn submit(
            &mut self,
            _id: RequestId,
            _request: Request,
            _accepted_at: Instant,
        ) -> Option<Completion> {
            panic!("deliberate spawned reactor panic");
        }

        fn cancel(&mut self, _id: RequestId) {}

        fn poll(&mut self, _deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
            Ok(Vec::new())
        }

        fn shutdown(&mut self) -> Result<(), ShutdownError> {
            Ok(())
        }
    }

    impl Backend for PanicOnStreamSubmitBackend {
        fn submit(
            &mut self,
            _id: RequestId,
            _request: Request,
            _accepted_at: Instant,
        ) -> Option<Completion> {
            None
        }

        fn submit_stream(
            &mut self,
            _id: RequestId,
            _request: StreamRequest,
            response: ResponseSink,
            _accepted_at: Instant,
        ) {
            self.response = Some(response);
            panic!("deliberate spawned stream reactor panic");
        }

        fn cancel(&mut self, _id: RequestId) {}

        fn poll(&mut self, _deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
            Ok(Vec::new())
        }

        fn shutdown(&mut self) -> Result<(), ShutdownError> {
            Ok(())
        }

        fn supports_streaming(&self) -> bool {
            true
        }
    }

    impl Backend for PanicOnceBackend {
        fn submit(
            &mut self,
            _id: RequestId,
            _request: Request,
            _accepted_at: Instant,
        ) -> Option<Completion> {
            None
        }

        fn cancel(&mut self, _id: RequestId) {}

        fn poll(&mut self, _deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
            if self.should_panic {
                self.should_panic = false;
                panic!("deliberate drive panic");
            }
            Ok(Vec::new())
        }

        fn shutdown(&mut self) -> Result<(), ShutdownError> {
            Ok(())
        }
    }

    #[test]
    fn failed_backend_shutdown_seals_and_remains_retryable() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut engine = Engine::with_backend(
            EngineConfig::manual(),
            Box::new(FailOnceBackend {
                shutdown_attempts: Arc::clone(&attempts),
            }),
        )
        .expect("Engine must construct");

        assert!(engine.stop_network().is_err());
        assert!(engine.shared.stopped.load(Ordering::Acquire));
        assert!(
            engine
                .dispatcher
                .as_ref()
                .is_some_and(|owner| owner.is_sealed())
        );

        engine
            .stop_network()
            .expect("a second teardown attempt must reach the backend");
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn consuming_shutdown_drop_retries_failed_backend_teardown() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let engine = Engine::with_backend(
            EngineConfig::manual(),
            Box::new(FailOnceBackend {
                shutdown_attempts: Arc::clone(&attempts),
            }),
        )
        .expect("Engine must construct");

        assert!(engine.shutdown().is_err());
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn spawned_shutdown_failure_is_remembered_after_join_handle_is_consumed() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut engine = Engine::with_backend(
            EngineConfig::spawned(),
            Box::new(FailOnceBackend {
                shutdown_attempts: Arc::clone(&attempts),
            }),
        )
        .expect("Engine must construct");

        assert!(engine.stop_network().is_err());
        assert!(engine.stop_network().is_err());
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn drive_guard_clears_reentrancy_flag_during_unwind() {
        let mut engine = Engine::with_backend(
            EngineConfig::manual(),
            Box::new(PanicOnceBackend { should_panic: true }),
        )
        .expect("Engine must construct");

        let panic = catch_unwind(AssertUnwindSafe(|| engine.drive(Instant::now())));
        assert!(panic.is_err());

        let status = engine
            .drive(Instant::now())
            .expect("drive must be usable after a contained backend panic");
        assert_eq!(status, DriveStatus::DeadlineReached);
    }

    #[test]
    fn spawned_reactor_panic_fails_accepted_waiters_instead_of_stranding_them() {
        let engine = Engine::with_backend(EngineConfig::spawned(), Box::new(PanicOnSubmitBackend))
            .expect("Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get("https://example.invalid/")
                    .build()
                    .expect("request must build"),
            )
            .expect("request must be accepted before the reactor panic");

        match pending.wait_for(Duration::from_secs(1)) {
            crate::WaitOutcome::Completed(Completion::Failed(error)) => {
                assert_eq!(error.kind(), ErrorKind::Internal);
                assert_eq!(error.message(), "NBReq reactor thread panicked");
            }
            other => panic!("reactor panic stranded or miscompleted the waiter: {other:?}"),
        }
        let shutdown = engine
            .shutdown()
            .expect_err("the contained reactor panic must remain observable at shutdown");
        assert_eq!(shutdown.error().kind(), ErrorKind::Internal);
    }

    #[test]
    fn spawned_reactor_panic_beats_backend_sink_drop_for_stream_readers() {
        let engine = Engine::with_backend(
            EngineConfig::spawned(),
            Box::new(PanicOnStreamSubmitBackend { response: None }),
        )
        .expect("Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get("http://example.test/")
                    .build()
                    .expect("stream request must build"),
            )
            .expect("stream request must be accepted before the reactor panic");

        match reader.wait_head() {
            Err(StreamError::Failed(error)) => {
                assert_eq!(error.kind(), ErrorKind::Internal);
                assert_eq!(error.message(), "NBReq reactor thread panicked");
            }
            other => panic!("reactor panic did not win the stream terminal race: {other:?}"),
        }
        let shutdown = engine
            .shutdown()
            .expect_err("the contained stream reactor panic must remain observable at shutdown");
        assert_eq!(shutdown.error().kind(), ErrorKind::Internal);
    }

    #[test]
    fn builder_sets_engine_configuration() {
        let workers = NonZeroUsize::new(3).expect("three is non-zero");
        let command_capacity = NonZeroUsize::new(7).expect("seven is non-zero");
        let callback_capacity = NonZeroUsize::new(9).expect("nine is non-zero");
        let connection_capacity = NonZeroUsize::new(11).expect("eleven is non-zero");
        let origin_capacity = NonZeroUsize::new(5).expect("five is non-zero");
        let idle_timeout = Duration::from_secs(17);
        let engine = EngineBuilder::spawned()
            .callback_workers(workers)
            .max_inflight_requests(command_capacity)
            .command_queue_capacity(command_capacity)
            .callback_queue_capacity(callback_capacity)
            .max_connections(connection_capacity)
            .max_connections_per_origin(origin_capacity)
            .max_idle_connections(6)
            .max_idle_connections_per_origin(2)
            .idle_connection_timeout(idle_timeout)
            .build()
            .expect("Engine must construct");
        assert_eq!(
            engine.config.callback_dispatch(),
            CallbackDispatch::Workers(workers)
        );
        assert_eq!(engine.config.max_inflight_requests(), command_capacity);
        assert_eq!(engine.config.command_queue_capacity(), command_capacity);
        assert_eq!(engine.config.callback_queue_capacity(), callback_capacity);
        assert_eq!(engine.config.max_connections(), connection_capacity);
        assert_eq!(engine.config.max_connections_per_origin(), origin_capacity);
        assert_eq!(engine.config.max_idle_connections(), 6);
        assert_eq!(engine.config.max_idle_connections_per_origin(), 2);
        assert_eq!(engine.config.idle_connection_timeout(), idle_timeout);
    }
}
