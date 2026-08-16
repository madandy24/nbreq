//! Private transport boundary. Backend implementation types never enter the public API.

use std::time::{Duration, Instant};

#[cfg(all(feature = "curl", any(test, feature = "test-support")))]
use crate::registry::Shared;
use crate::{Completion, Error, Request, RequestId, ShutdownError};
#[cfg(all(feature = "curl", any(test, feature = "test-support")))]
use std::sync::Arc;

#[cfg(all(feature = "curl", any(test, feature = "test-support")))]
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
    Interruptible,
}

pub(crate) trait Backend {
    fn submit(&mut self, id: RequestId, request: Request) -> Option<Completion>;
    fn cancel(&mut self, id: RequestId);
    fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error>;
    fn shutdown(&mut self) -> Result<(), ShutdownError>;

    fn poll_mode(&self) -> PollMode {
        PollMode::CommandDriven
    }
}

#[cfg(all(feature = "curl", any(test, feature = "test-support")))]
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

#[cfg(all(feature = "curl", any(test, feature = "test-support")))]
pub(crate) fn curl_factory() -> Box<dyn BackendFactory> {
    Box::new(curl::CurlFactory::new())
}

pub(crate) fn long_poll_deadline() -> Instant {
    Instant::now()
        .checked_add(Duration::from_secs(24 * 60 * 60))
        .unwrap_or_else(Instant::now)
}
