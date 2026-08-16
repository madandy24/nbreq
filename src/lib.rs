//! Runtime-independent HTTP client architecture.
//!
//! The crate implements NBReq's backend-independent ownership and lifecycle kernel. WP2 adds a
//! deliberately private curl Multi proving backend; the ordinary public constructor still uses the
//! deterministic scaffold until the transport contract is ready for consumers.

mod backend;
mod callback;
mod client;
mod context;
mod dispatch;
mod engine;
mod reactor;
mod registry;
mod types;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(all(test, feature = "curl"))]
mod curl_tests;
#[cfg(test)]
mod lifecycle_tests;

pub use callback::{DetachedCallbacks, ShutdownOutcome};
pub use client::{CancelOnDrop, Client, PendingRequest, RequestHandle, WaitOutcome};
pub use engine::{Engine, EngineBuilder};
pub use types::{
    CallbackDispatch, Completion, DriveStatus, EngineConfig, Error, ErrorKind, ExecuteError,
    Header, Method, Request, RequestBuilder, RequestId, RequestOptions, Response, RunMode,
    ShutdownError, TlsVerification,
};
