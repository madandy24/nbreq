use std::cell::Cell;
use std::marker::PhantomData;
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

        self.driving = true;
        let result = match self.backend.as_mut() {
            Some(backend) => backend.drive(deadline),
            None => Err(Error::new(ErrorKind::EngineStopped, "Engine has stopped")),
        };
        self.driving = false;
        result
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
        if self.shared.stopped.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        if let Some(mut backend) = self.backend.take() {
            backend.shutdown()?;
        }
        self.dispatcher.seal();
        Ok(())
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

    /// Builds one independent Engine.
    pub fn build(self) -> Result<Engine, Error> {
        Engine::new(self.config)
    }
}
