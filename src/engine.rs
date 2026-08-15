use std::cell::Cell;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::backend::{self, Backend};
use crate::dispatch::ScaffoldDispatcher;
use crate::{
    Client, DriveStatus, EngineConfig, Error, ErrorKind, RunMode, ShutdownError, ShutdownOutcome,
};

static NEXT_ENGINE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) id: u64,
    pub(crate) stopped: AtomicBool,
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
    backend: Option<Box<dyn Backend>>,
    dispatcher: ScaffoldDispatcher,
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
    pub fn new(config: EngineConfig) -> Result<Self, Error> {
        let id = NEXT_ENGINE_ID.fetch_add(1, Ordering::Relaxed);
        if id == u64::MAX {
            return Err(Error::new(
                ErrorKind::Internal,
                "Engine identity space is exhausted",
            ));
        }

        Ok(Self {
            shared: Arc::new(Shared {
                id,
                stopped: AtomicBool::new(false),
            }),
            config,
            backend: Some(backend::scaffold()),
            dispatcher: ScaffoldDispatcher::default(),
            driving: false,
            _not_sync: PhantomData,
        })
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

    /// Cancels requests accepted before the cancellation barrier while keeping the Engine alive.
    ///
    /// WP1 installs the request registry, barrier, and reactor wakeup. WP0 accepts no requests, so
    /// this is currently an intentionally harmless operation.
    pub fn cancel_all(&self) {}

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

        let (driving, backend) = (&mut self.driving, &mut self.backend);
        *driving = true;
        let _guard = DrivingGuard { driving };
        match backend.as_mut() {
            Some(backend) => backend.drive(deadline),
            None => Err(Error::new(ErrorKind::EngineStopped, "Engine has stopped")),
        }
    }

    /// Irreversibly stops network work and waits for callback dispatch to drain.
    pub fn shutdown(mut self) -> Result<(), ShutdownError> {
        self.stop_network()
    }

    /// Irreversibly stops network work, then waits up to `duration` for callback dispatch.
    pub fn shutdown_for(mut self, _duration: Duration) -> Result<ShutdownOutcome, ShutdownError> {
        self.stop_network()?;
        // The WP0 scaffold cannot accept callbacks, so its sealed callback domain is complete.
        Ok(ShutdownOutcome::Complete)
    }

    fn stop_network(&mut self) -> Result<(), ShutdownError> {
        // Closing admission and completing teardown are separate facts. A failed teardown remains
        // retryable by the consuming shutdown path's Drop fallback.
        self.shared.stopped.store(true, Ordering::Release);
        let shutdown = match self.backend.as_mut() {
            Some(backend) => backend.shutdown(),
            None => Ok(()),
        };

        // WP1 will publish every terminal callback event before reaching this seal. Sealing must
        // still happen when backend teardown reports failure so no producer can add later work.
        self.dispatcher.seal();

        if shutdown.is_ok() {
            self.backend = None;
        }
        shutdown
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ignored = self.stop_network();
    }
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

    use super::*;

    struct FailOnceBackend {
        shutdown_attempts: Arc<AtomicUsize>,
    }

    impl Backend for FailOnceBackend {
        fn drive(&mut self, _deadline: Instant) -> Result<DriveStatus, Error> {
            Ok(DriveStatus::Idle)
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

    impl Backend for PanicOnceBackend {
        fn drive(&mut self, _deadline: Instant) -> Result<DriveStatus, Error> {
            if self.should_panic {
                self.should_panic = false;
                panic!("deliberate drive panic");
            }
            Ok(DriveStatus::Idle)
        }

        fn shutdown(&mut self) -> Result<(), ShutdownError> {
            Ok(())
        }
    }

    #[test]
    fn failed_backend_shutdown_seals_and_remains_retryable() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut engine = Engine::new(EngineConfig::manual()).expect("Engine must construct");
        engine.backend = Some(Box::new(FailOnceBackend {
            shutdown_attempts: Arc::clone(&attempts),
        }));

        assert!(engine.stop_network().is_err());
        assert!(engine.shared.stopped.load(Ordering::Acquire));
        assert!(engine.dispatcher.is_sealed());
        assert!(engine.backend.is_some());

        engine
            .stop_network()
            .expect("a second teardown attempt must reach the backend");
        assert!(engine.backend.is_none());
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn consuming_shutdown_drop_retries_failed_backend_teardown() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut engine = Engine::new(EngineConfig::manual()).expect("Engine must construct");
        engine.backend = Some(Box::new(FailOnceBackend {
            shutdown_attempts: Arc::clone(&attempts),
        }));

        assert!(engine.shutdown().is_err());
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn drive_guard_clears_reentrancy_flag_during_unwind() {
        let mut engine = Engine::new(EngineConfig::manual()).expect("Engine must construct");
        engine.backend = Some(Box::new(PanicOnceBackend { should_panic: true }));

        let panic = catch_unwind(AssertUnwindSafe(|| engine.drive(Instant::now())));
        assert!(panic.is_err());

        let status = engine
            .drive(Instant::now())
            .expect("drive must be usable after a contained backend panic");
        assert_eq!(status, DriveStatus::Idle);
    }

    #[test]
    fn builder_sets_callback_worker_count() {
        let workers = NonZeroUsize::new(3).expect("three is non-zero");
        let engine = EngineBuilder::spawned()
            .callback_workers(workers)
            .build()
            .expect("Engine must construct");
        assert_eq!(
            engine.config.callback_dispatch(),
            crate::CallbackDispatch::Workers(workers)
        );
    }
}
