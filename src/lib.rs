//! Runtime-independent HTTP client architecture.
//!
//! This WP0 crate establishes NBReq's public ownership and lifecycle boundary. It intentionally
//! contains no production HTTP backend yet. The private scaffold backend exists so that the API,
//! examples, and feature matrix compile before curl or native implementation details are admitted.

mod backend;
mod callback;
mod client;
mod dispatch;
mod engine;
mod types;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use callback::{DetachedCallbacks, ShutdownOutcome};
pub use client::{CancelOnDrop, Client, PendingRequest, RequestHandle, WaitOutcome};
pub use engine::{Engine, EngineBuilder};
pub use types::{
    CallbackDispatch, Completion, DriveStatus, EngineConfig, Error, ErrorKind, ExecuteError,
    Header, Method, Request, RequestBuilder, RequestId, RequestOptions, Response, RunMode,
    ShutdownError,
};
