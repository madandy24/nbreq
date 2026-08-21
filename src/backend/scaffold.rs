#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
use std::time::Instant;

#[cfg(any(test, feature = "test-support"))]
use crate::StreamRequest;
#[cfg(any(test, feature = "test-support"))]
use crate::stream::ResponseSink;
use crate::{Completion, Error, ErrorKind, Request, RequestId, ShutdownError};

use super::{Backend, BackendCompletion};

#[cfg_attr(feature = "curl-pilot", allow(dead_code))]
pub(super) struct ScaffoldBackend;

#[cfg_attr(feature = "curl-pilot", allow(dead_code))]
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
#[derive(Default)]
pub(super) struct HeldBackend {
    streams: HashMap<RequestId, ResponseSink>,
}

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

    fn submit_stream(
        &mut self,
        id: RequestId,
        _request: StreamRequest,
        response: ResponseSink,
        _accepted_at: Instant,
    ) {
        self.streams.insert(id, response);
    }

    fn cancel(&mut self, id: RequestId) {
        if let Some(mut response) = self.streams.remove(&id) {
            response.cancel();
        }
    }

    fn poll(&mut self, _deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
        Ok(Vec::new())
    }

    fn shutdown(&mut self) -> Result<(), ShutdownError> {
        for (_id, mut response) in self.streams.drain() {
            response.cancel();
        }
        Ok(())
    }

    fn supports_streaming(&self) -> bool {
        true
    }
}
