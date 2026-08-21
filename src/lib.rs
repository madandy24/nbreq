//! Runtime-independent HTTP client architecture.
//!
//! The crate implements NBReq's backend-independent ownership and lifecycle kernel. Enabling the
//! `curl-pilot` feature makes the ordinary [`Engine::new`] constructor use the private curl Multi
//! backend without exposing backend implementation types.

mod backend;
mod callback;
mod client;
mod context;
mod dispatch;
mod engine;
mod metrics;
mod reactor;
mod registry;
mod stream;
mod types;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(all(fuzzing, feature = "native"))]
#[doc(hidden)]
pub mod fuzzing;

#[cfg(all(test, feature = "curl-pilot"))]
mod curl_tests;
#[cfg(test)]
mod lifecycle_tests;

pub use callback::{DetachedCallbacks, ShutdownOutcome};
pub use client::{CancelOnDrop, Client, PendingRequest, RequestHandle, WaitOutcome};
pub use engine::{Engine, EngineBuilder};
pub use metrics::{EngineMetrics, ResourceMetrics};
pub use stream::{
    ResponseHead, ResponseReader, StreamError, StreamRead, StreamRequest, StreamRequestBuilder,
    TryPushError, TryPushErrorKind, UploadBody, UploadFinishError, UploadFinishErrorKind,
    UploadSender,
};
pub use types::{
    CallbackDispatch, Completion, DriveStatus, EngineConfig, Error, ErrorKind, ExecuteError,
    Header, LimitKind, Method, Request, RequestBuilder, RequestId, RequestOptions, Response,
    RunMode, ShutdownError, TimeoutKind, TlsVerification, TransportStage,
};
