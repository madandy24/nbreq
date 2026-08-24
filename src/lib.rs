#![warn(missing_docs)]

//! Runtime-independent HTTP client architecture.
//!
//! The crate implements NBReq's backend-independent ownership and lifecycle kernel. Compiled HTTP
//! implementations can be selected explicitly with [`EngineBuilder::http_backend`]. The default
//! build and ordinary [`Engine::new`] constructor use NBReq's native HTTP implementation.
//!
//! [`Engine::resolver`] and [`Engine::tcp_connector`] issue cloneable capability tickets into the
//! same Engine lifecycle. The F0 public DNS/TCP surface is compile-checked; operations currently
//! fail [`ErrorKind::Unsupported`] before admission and are not connected to the private resolver
//! or reactor.
#![doc = include_str!("../docs/getting-started.md")]

mod atomic;
mod backend;
mod callback;
mod client;
mod context;
mod dispatch;
mod dns;
mod engine;
mod metrics;
mod reactor;
mod registry;
mod stream;
mod tcp;
mod types;
mod waiter;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(all(fuzzing, feature = "native", feature = "test-support"))]
#[doc(hidden)]
pub mod fuzzing;

#[cfg(test)]
mod lifecycle_tests;

pub use callback::{DetachedCallbacks, ShutdownOutcome};
pub use client::{CancelOnDrop, Client, PendingRequest, RequestHandle, WaitOutcome};
pub use dns::{
    AddressFamily, AddressOrder, CacheMode, PendingResolve, ResolveCompletion, ResolveHandle,
    ResolveRequest, ResolveRequestBuilder, ResolveResponse, ResolveStatus, ResolveWaitOutcome,
    ResolvedAddress, Resolver,
};
pub use engine::{Engine, EngineBuilder};
pub use metrics::{EngineMetrics, ResourceMetrics};
pub use stream::{
    ResponseHead, ResponseReader, StreamError, StreamRead, StreamRequest, StreamRequestBuilder,
    TryPushError, TryPushErrorKind, UploadBody, UploadFinishError, UploadFinishErrorKind,
    UploadSender,
};
pub use tcp::{
    PendingTcpConnect, TcpConnectCompletion, TcpConnectHandle, TcpConnectRequest,
    TcpConnectRequestBuilder, TcpConnectTarget, TcpConnectWaitOutcome, TcpConnection,
    TcpConnectionHandle, TcpConnector, TcpFinishError, TcpRead, TcpReader, TcpSendError,
    TcpSendErrorKind, TcpStreamError, TcpWriter,
};
pub use types::{
    CallbackDispatch, Completion, DnsFailure, DriveStatus, EngineConfig, Error, ErrorKind,
    ExecuteError, Header, HttpBackend, LimitKind, Method, Request, RequestBuilder, RequestId,
    RequestOptions, Response, RunMode, ShutdownError, TimeoutKind, TlsFailure, TlsVerification,
    TransportStage,
};
pub use waiter::WaiterTarget;
