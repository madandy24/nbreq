#![warn(missing_docs)]

//! Runtime-independent HTTP client architecture.
//!
//! The crate implements NBReq's backend-independent ownership and lifecycle kernel. Compiled HTTP
//! implementations can be selected explicitly with [`EngineBuilder::http_backend`]. The default
//! build and ordinary [`Engine::new`] constructor use NBReq's native HTTP implementation.
//!
//! [`Engine::tcp_connector`] issues a cloneable capability ticket into the same Engine lifecycle.
//! The default-on `resolver` feature additionally exposes public hostname resolution through
//! `Engine::resolver`. Both capabilities use the Engine-owned native DNS and reactor owners.
#![cfg_attr(
    not(feature = "resolver"),
    doc = r#"

The public Resolver API is absent when the `resolver` feature is disabled:

```compile_fail
use nbreq::{Engine, Resolver};

fn public_resolver_is_not_compiled(engine: &Engine) {
    let _: Resolver = engine.resolver();
}
```
"#
)]
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

#[cfg(all(test, feature = "native"))]
mod dns_wiring_tests;

pub use callback::{DetachedCallbacks, ShutdownOutcome};
pub use client::{CancelOnDrop, Client, PendingRequest, RequestHandle, WaitOutcome};
#[cfg(feature = "resolver")]
pub use dns::{
    AddressFamily, AddressOrder, CacheMode, PendingResolve, ResolveCompletion, ResolveHandle,
    ResolveRequest, ResolveRequestBuilder, ResolveResponse, ResolveStatus, ResolveWaitOutcome,
    ResolvedAddress, Resolver,
};
#[cfg(all(not(feature = "resolver"), feature = "native"))]
pub(crate) use dns::{
    AddressFamily, AddressOrder, CacheMode, ResolveResponse, ResolveStatus, ResolvedAddress,
};
#[cfg(not(feature = "resolver"))]
pub(crate) use dns::{ResolveCompletion, ResolveRequest};
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
    TcpConnectionHandle, TcpConnector, TcpFinishError, TcpFinishStatus, TcpRead, TcpReader,
    TcpSendError, TcpSendErrorKind, TcpStreamError, TcpWriter,
};
pub use types::{
    CallbackDispatch, Completion, DnsFailure, DriveStatus, EngineConfig, Error, ErrorKind,
    ExecuteError, Header, HttpBackend, LimitKind, Method, Request, RequestBuilder, RequestId,
    RequestOptions, Response, RunMode, ShutdownError, TimeoutKind, TlsFailure, TlsVerification,
    TransportStage,
};
pub use waiter::WaiterTarget;
