//! Controls for lifecycle, adapter, and private transport proof tests.
//!
//! This module is available to NBReq's own tests and through the opt-in `test-support` feature. It
//! exposes no transport implementation types. The ordinary controller is deterministic and
//! network-free; the curl constructor is an explicitly experimental exception.

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

/// Creates an Engine using the private curl Multi proving backend.
///
/// This is experimental test support rather than a stable backend-selection API. It exists only
/// when both the `curl-pilot` and `test-support` features are enabled (or in NBReq's own curl tests).
#[cfg(feature = "curl-pilot")]
pub fn curl_engine(config: EngineConfig) -> Result<Engine, Error> {
    Engine::with_curl_backend(config)
}

#[cfg(all(test, feature = "curl-pilot"))]
pub(crate) fn curl_engine_with_test_ca(
    config: EngineConfig,
    ca_pem: Vec<u8>,
) -> Result<Engine, Error> {
    Engine::with_curl_test_ca(config, ca_pem)
}
