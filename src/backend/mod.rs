//! Private transport boundary. Backend implementation types never enter the public API.

use std::time::{Duration, Instant};

#[cfg(all(feature = "curl-pilot", any(test, feature = "test-support")))]
use crate::EngineConfig;
#[cfg(all(feature = "curl-pilot", any(test, feature = "test-support")))]
use crate::registry::Shared;
use crate::{Completion, Error, Request, RequestId, ShutdownError};
#[cfg(all(feature = "curl-pilot", any(test, feature = "test-support")))]
use std::sync::Arc;

#[cfg(all(feature = "curl-pilot", any(test, feature = "test-support")))]
mod curl;
#[cfg(feature = "native")]
mod native;
mod scaffold;

pub(crate) struct BackendCompletion {
    pub(crate) id: RequestId,
    pub(crate) completion: Completion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PollMode {
    CommandDriven,
    #[allow(dead_code)] // Constructed only when a real transport feature is enabled.
    Interruptible {
        max_wait: Duration,
    },
}

#[cfg(all(feature = "curl-pilot", any(test, feature = "test-support")))]
#[derive(Clone, Copy)]
pub(crate) struct ResponseLimits {
    pub(crate) body_bytes: usize,
    pub(crate) header_bytes: usize,
    pub(crate) header_count: usize,
}

pub(crate) trait Backend {
    fn submit(
        &mut self,
        id: RequestId,
        request: Request,
        accepted_at: Instant,
    ) -> Option<Completion>;
    fn cancel(&mut self, id: RequestId);
    fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error>;
    fn shutdown(&mut self) -> Result<(), ShutdownError>;

    fn poll_mode(&self) -> PollMode {
        PollMode::CommandDriven
    }
}

#[cfg(all(feature = "curl-pilot", any(test, feature = "test-support")))]
pub(crate) trait BackendFactory: Send {
    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error>;
}

pub(crate) fn scaffold() -> Box<dyn Backend + Send> {
    Box::new(scaffold::ScaffoldBackend)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn held() -> Box<dyn Backend + Send> {
    Box::new(scaffold::HeldBackend)
}

#[cfg(all(feature = "curl-pilot", any(test, feature = "test-support")))]
pub(crate) fn curl_factory(config: &EngineConfig) -> Box<dyn BackendFactory> {
    Box::new(curl::CurlFactory::new(ResponseLimits {
        body_bytes: config.max_response_body_bytes(),
        header_bytes: config.max_header_bytes(),
        header_count: config.max_header_count(),
    }))
}

pub(crate) fn interruptible_poll_deadline(max_wait: Duration) -> Instant {
    Instant::now()
        .checked_add(max_wait)
        .unwrap_or_else(Instant::now)
}
