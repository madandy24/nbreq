//! Runtime-independent HTTP client architecture.
//!
//! The WP1 crate implements NBReq's backend-independent ownership and lifecycle kernel. It
//! intentionally contains no production HTTP backend yet. The private scaffold backend keeps the
//! state machine deterministic before curl or native transport details are admitted.

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

#[cfg(test)]
mod lifecycle_tests;

pub use callback::{DetachedCallbacks, ShutdownOutcome};
pub use client::{CancelOnDrop, Client, PendingRequest, RequestHandle, WaitOutcome};
pub use engine::{Engine, EngineBuilder};
pub use types::{
    CallbackDispatch, Completion, DriveStatus, EngineConfig, Error, ErrorKind, ExecuteError,
    Header, Method, Request, RequestBuilder, RequestId, RequestOptions, Response, RunMode,
    ShutdownError,
};
