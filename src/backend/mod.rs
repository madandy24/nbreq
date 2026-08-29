//! Private transport boundary. Backend implementation types never enter the public API.

use std::time::{Duration, Instant};

use crate::metrics::Metrics;
use crate::registry::Shared;
use crate::registry::TcpConnectSink;
use crate::stream::ResponseSink;
use crate::{
    Completion, Error, ErrorKind, Request, RequestId, ResolveCompletion, ResolveRequest,
    ShutdownError, StreamRequest, TcpConnectRequest,
};
use std::sync::Arc;

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
mod native_dns;
#[cfg(feature = "native")]
mod native_dns_config;
#[cfg(feature = "native")]
mod native_http;
#[cfg(feature = "native")]
mod native_poll;
#[cfg(all(fuzzing, feature = "native", feature = "test-support"))]
pub(crate) use native_dns::fuzz_dns_response;
#[cfg(all(fuzzing, feature = "native", feature = "test-support"))]
pub(crate) use native_http::{fuzz_response_decoder, fuzz_streaming_response_decoder};
#[cfg(feature = "native")]
mod native_tls;
mod scaffold;

pub(crate) struct BackendCompletion {
    pub(crate) id: RequestId,
    pub(crate) completion: Completion,
}

pub(crate) struct BackendResolveCompletion {
    pub(crate) id: RequestId,
    pub(crate) completion: ResolveCompletion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PollMode {
    CommandDriven,
    #[allow(dead_code)] // Constructed only when a real transport feature is enabled.
    Interruptible {
        max_wait: Duration,
    },
}

#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) trait Backend {
    fn attach_metrics(&mut self, _metrics: Arc<Metrics>) {}

    fn connection_metrics_available(&self) -> bool {
        false
    }

    fn submit(
        &mut self,
        id: RequestId,
        request: Request,
        accepted_at: Instant,
    ) -> Option<Completion>;
    fn submit_stream(
        &mut self,
        _id: RequestId,
        _request: StreamRequest,
        mut response: ResponseSink,
        _accepted_at: Instant,
    ) {
        response.fail(Error::new(
            crate::ErrorKind::Unsupported,
            "this backend does not support streaming requests",
        ));
    }
    fn cancel(&mut self, id: RequestId);
    fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error>;
    fn shutdown(&mut self) -> Result<(), ShutdownError>;

    fn submit_resolve(
        &mut self,
        _id: RequestId,
        _request: ResolveRequest,
        _accepted_at: Instant,
        _max_results: usize,
    ) -> Option<ResolveCompletion> {
        Some(ResolveCompletion::Failed(Error::new(
            ErrorKind::Unsupported,
            "public hostname resolution is not available on this Engine",
        )))
    }

    fn poll_resolves(&mut self) -> Result<Vec<BackendResolveCompletion>, Error> {
        Ok(Vec::new())
    }

    fn submit_tcp_connect(
        &mut self,
        _request: TcpConnectRequest,
        sink: TcpConnectSink,
        _accepted_at: Instant,
    ) {
        sink.fail(Error::new(
            ErrorKind::Unsupported,
            "standalone TCP connections are not available on this Engine",
        ));
    }

    fn poll_mode(&self) -> PollMode {
        PollMode::CommandDriven
    }

    fn wants_poll_without_requests(&self) -> bool {
        false
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_public_resolver(&self) -> bool {
        false
    }

    fn supports_standalone_tcp(&self) -> bool {
        false
    }
}

#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) trait BackendFactory: Send {
    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error>;

    fn connection_metrics_available(&self) -> bool {
        false
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_public_resolver(&self) -> bool {
        false
    }

    fn supports_standalone_tcp(&self) -> bool {
        false
    }
}

#[allow(dead_code)] // Internal lifecycle-test backend; never an ordinary public runtime.
pub(crate) fn scaffold() -> Box<dyn Backend + Send> {
    Box::new(scaffold::ScaffoldBackend)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn held() -> Box<dyn Backend + Send> {
    Box::new(scaffold::HeldBackend::default())
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_http_factory(config: &crate::EngineConfig) -> Box<dyn BackendFactory> {
    Box::new(native_http::NativeHttpFactory::new(config))
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
#[allow(dead_code)]
pub(crate) fn native_http_backend(
    config: &crate::EngineConfig,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new(config).into_backend()
}

#[cfg(all(test, feature = "native"))]
pub(crate) fn native_http_backend_with_write_limit(
    config: &crate::EngineConfig,
    bytes: usize,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new(config).into_backend_with_write_limit(bytes)
}

#[cfg(all(test, feature = "native"))]
pub(crate) fn native_http_backend_with_standalone_socket_gate(
    config: &crate::EngineConfig,
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new(config)
        .into_backend_with_standalone_socket_gate(entered, release)
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_http_factory_with_nameserver(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
) -> Box<dyn BackendFactory> {
    Box::new(native_http::NativeHttpFactory::new_with_nameserver(
        config, nameserver,
    ))
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_http_factory_with_nameserver_and_search_suffixes(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
    suffixes: impl IntoIterator<Item = impl AsRef<str>>,
) -> Box<dyn BackendFactory> {
    Box::new(
        native_http::NativeHttpFactory::new_with_nameserver_and_search_suffixes(
            config, nameserver, suffixes,
        ),
    )
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_http_backend_with_nameserver(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new_with_nameserver(config, nameserver).into_backend()
}

#[cfg(all(test, feature = "native"))]
pub(crate) fn native_http_backend_with_nameserver_and_failed_standalone_addresses(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
    count: usize,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new_with_nameserver(config, nameserver)
        .into_backend_with_failed_standalone_addresses(count)
}

#[cfg(all(test, feature = "native"))]
pub(crate) fn native_http_backend_with_nameserver_and_delayed_failed_standalone_address(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
    delay: std::time::Duration,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new_with_nameserver(config, nameserver)
        .into_backend_with_delayed_failed_standalone_address(delay)
}

#[cfg(all(test, feature = "native"))]
pub(crate) fn native_http_backend_with_nameserver_and_standalone_dns_handoff_gate(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new_with_nameserver(config, nameserver)
        .into_backend_with_standalone_dns_handoff_gate(entered, release)
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_http_factory_with_system_dns(
    config: &crate::EngineConfig,
) -> Result<Box<dyn BackendFactory>, Error> {
    Ok(Box::new(
        native_http::NativeHttpFactory::new_with_system_dns(config)?,
    ))
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_https_factory_with_nameserver(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
) -> Result<Box<dyn BackendFactory>, Error> {
    Ok(Box::new(
        native_http::NativeHttpFactory::new_with_nameserver_and_platform_tls(config, nameserver)?,
    ))
}

#[cfg(feature = "native")]
pub(crate) fn native_https_factory_with_system_dns(
    config: &crate::EngineConfig,
) -> Result<Box<dyn BackendFactory>, Error> {
    Ok(Box::new(
        native_http::NativeHttpFactory::new_with_system_dns_and_platform_tls(config)?,
    ))
}

#[cfg(feature = "native")]
pub(crate) fn native_https_backend_with_system_dns(
    config: &crate::EngineConfig,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new_with_system_dns_and_platform_tls(config)?.into_backend()
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_https_factory_with_nameserver_and_test_root(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
    root_der: Vec<u8>,
) -> Result<Box<dyn BackendFactory>, Error> {
    Ok(Box::new(
        native_http::NativeHttpFactory::new_with_nameserver_and_test_root(
            config, nameserver, root_der,
        )?,
    ))
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_https_backend_with_nameserver_and_test_root(
    config: &crate::EngineConfig,
    nameserver: std::net::SocketAddr,
    root_der: Vec<u8>,
) -> Result<Box<dyn Backend + Send>, Error> {
    native_http::NativeHttpFactory::new_with_nameserver_and_test_root(config, nameserver, root_der)?
        .into_backend()
}

pub(crate) fn interruptible_poll_deadline(max_wait: Duration) -> Instant {
    Instant::now()
        .checked_add(max_wait)
        .unwrap_or_else(Instant::now)
}
