use std::time::Instant;

use crate::{Completion, Error, ErrorKind, Request, RequestId, ShutdownError};

use super::{Backend, BackendCompletion};

pub(super) struct ScaffoldBackend;

impl Backend for ScaffoldBackend {
    fn submit(
        &mut self,
        _id: RequestId,
        _request: Request,
        _accepted_at: Instant,
    ) -> Option<Completion> {
        Some(Completion::Failed(Error::new(
            ErrorKind::BackendUnavailable,
            "no production HTTP backend is implemented",
        )))
    }

    fn cancel(&mut self, _id: RequestId) {}

    fn poll(&mut self, _deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
        Ok(Vec::new())
    }

    fn shutdown(&mut self) -> Result<(), ShutdownError> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(super) struct HeldBackend;

#[cfg(any(test, feature = "test-support"))]
impl Backend for HeldBackend {
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
        Ok(())
    }
}
