//! Deterministic controls for lifecycle and adapter tests.
//!
//! This module is available to NBReq's own tests and through the opt-in `test-support` feature. It
//! exposes no transport implementation types and performs no network access.

use std::sync::{Arc, Weak};

use crate::registry::Shared;
use crate::{Completion, Engine, EngineConfig, Error, RequestId};

/// Observer/controller for the deterministic held-request backend.
#[derive(Clone, Debug)]
pub struct TestController {
    shared: Weak<Shared>,
}

impl TestController {
    /// Attempts to commit one canonical terminal result, returning whether it won the race.
    pub fn complete(&self, id: RequestId, completion: Completion) -> bool {
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let won = shared.complete_id(id, completion);
        shared.queue.wake();
        won
    }

    /// Returns the number of accepted requests that have not reached terminal state.
    #[must_use]
    pub fn active_requests(&self) -> usize {
        self.shared
            .upgrade()
            .map_or(0, |shared| shared.active_count())
    }
}

/// Creates an Engine whose accepted requests remain pending until cancelled, shut down, or
/// completed through the returned controller.
pub fn engine(config: EngineConfig) -> Result<(Engine, TestController), Error> {
    let engine = Engine::with_backend(config, crate::backend::held())?;
    let shared: Arc<Shared> = engine.shared_for_testing();
    let controller = TestController {
        shared: Arc::downgrade(&shared),
    };
    Ok((engine, controller))
}
