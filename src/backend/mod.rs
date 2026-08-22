//! Private transport boundary. Backend implementation types never enter the public API.

use std::time::{Duration, Instant};

#[cfg(feature = "curl-pilot")]
use crate::EngineConfig;
use crate::metrics::Metrics;
use crate::registry::Shared;
use crate::stream::ResponseSink;
use crate::{Completion, Error, Request, RequestId, ShutdownError, StreamRequest};
use std::sync::Arc;

#[cfg(feature = "curl-pilot")]
mod curl;
#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
mod native_dns;
#[cfg(feature = "native")]
mod native_dns_config;
#[cfg(feature = "native")]
mod native_http;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PollMode {
    CommandDriven,
    #[allow(dead_code)] // Constructed only when a real transport feature is enabled.
    Interruptible {
        max_wait: Duration,
    },
}

#[cfg(feature = "curl-pilot")]
#[derive(Clone, Copy)]
pub(crate) struct ResponseLimits {
    pub(crate) body_bytes: usize,
    pub(crate) header_bytes: usize,
    pub(crate) header_count: usize,
}

pub(crate) trait Backend {
    fn attach_metrics(&mut self, _metrics: Arc<Metrics>) {}

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

    fn poll_mode(&self) -> PollMode {
        PollMode::CommandDriven
    }

    fn wants_poll_without_requests(&self) -> bool {
        false
    }

    fn supports_streaming(&self) -> bool {
        false
    }
}

#[cfg_attr(not(feature = "curl-pilot"), allow(dead_code))]
pub(crate) trait BackendFactory: Send {
    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error>;

    fn supports_streaming(&self) -> bool {
        false
    }
}

#[cfg_attr(feature = "curl-pilot", allow(dead_code))]
pub(crate) fn scaffold() -> Box<dyn Backend + Send> {
    Box::new(scaffold::ScaffoldBackend)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn held() -> Box<dyn Backend + Send> {
    Box::new(scaffold::HeldBackend::default())
}

#[cfg(feature = "curl-pilot")]
pub(crate) fn curl_factory(config: &EngineConfig) -> Box<dyn BackendFactory> {
    Box::new(curl::CurlFactory::new(ResponseLimits {
        body_bytes: config.max_response_body_bytes(),
        header_bytes: config.max_header_bytes(),
        header_count: config.max_header_count(),
    }))
}

#[cfg(all(test, feature = "curl-pilot"))]
pub(crate) fn curl_factory_with_test_ca(
    config: &EngineConfig,
    ca_pem: Vec<u8>,
) -> Box<dyn BackendFactory> {
    Box::new(curl::CurlFactory::new_with_test_ca(
        ResponseLimits {
            body_bytes: config.max_response_body_bytes(),
            header_bytes: config.max_header_bytes(),
            header_count: config.max_header_count(),
        },
        ca_pem,
    ))
}

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_http_factory(config: &crate::EngineConfig) -> Box<dyn BackendFactory> {
    Box::new(native_http::NativeHttpFactory::new(config))
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

#[cfg(all(feature = "native", any(test, feature = "test-support")))]
pub(crate) fn native_https_factory_with_system_dns(
    config: &crate::EngineConfig,
) -> Result<Box<dyn BackendFactory>, Error> {
    Ok(Box::new(
        native_http::NativeHttpFactory::new_with_system_dns_and_platform_tls(config)?,
    ))
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
