//! Private transport boundary. Backend implementation types never enter the public API.

use std::time::Instant;

use crate::{Completion, Error, Request, RequestId, ShutdownError};

#[cfg(feature = "curl")]
mod curl;
#[cfg(feature = "native")]
mod native;
mod scaffold;

pub(crate) struct BackendCompletion {
    pub(crate) id: RequestId,
    pub(crate) completion: Completion,
}

pub(crate) trait Backend: Send {
    fn submit(&mut self, id: RequestId, request: Request) -> Option<Completion>;
    fn cancel(&mut self, id: RequestId);
    fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error>;
    fn shutdown(&mut self) -> Result<(), ShutdownError>;
}

pub(crate) fn scaffold() -> Box<dyn Backend> {
    Box::new(scaffold::ScaffoldBackend)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn held() -> Box<dyn Backend> {
    Box::new(scaffold::HeldBackend)
}
