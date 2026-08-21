//! Controls for lifecycle, adapter, and private transport proof tests.
//!
//! This module is available to NBReq's own tests and through the opt-in `test-support` feature. It
//! exposes no transport implementation types. The ordinary controller is deterministic and
//! network-free; the curl constructor is an explicitly experimental exception.

use std::sync::{Arc, Weak};

use crate::registry::Shared;
use crate::{Completion, Engine, EngineConfig, Error, RequestId};

/// Observer/controller for the deterministic held-request backend.
#[derive(Clone, Debug)]
pub struct TestController {
    shared: Weak<Shared>,
}

impl TestController {
    /// Attempts to commit one canonical terminal result, returning whether it won the race.
    pub fn complete(&self, id: RequestId, completion: Completion) -> bool {
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let won = shared.complete_id(id, completion);
        shared.queue.wake();
        won
    }

    /// Returns the number of accepted requests that have not reached terminal state.
    #[must_use]
    pub fn active_requests(&self) -> usize {
        self.shared
            .upgrade()
            .map_or(0, |shared| shared.active_count())
    }
}

/// Creates an Engine whose accepted requests remain pending until cancelled, shut down, or
/// completed through the returned controller.
pub fn engine(config: EngineConfig) -> Result<(Engine, TestController), Error> {
    let engine = Engine::with_backend(config, crate::backend::held())?;
    let shared: Arc<Shared> = engine.shared_for_testing();
    let controller = TestController {
        shared: Arc::downgrade(&shared),
    };
    Ok((engine, controller))
}

/// Creates an Engine using the private curl Multi proving backend.
///
/// This is experimental test support rather than a stable backend-selection API. It exists only
/// when both the `curl-pilot` and `test-support` features are enabled (or in NBReq's own curl tests).
#[cfg(feature = "curl-pilot")]
pub fn curl_engine(config: EngineConfig) -> Result<Engine, Error> {
    Engine::with_curl_backend(config)
}

/// Creates an Engine using the private Rust-native HTTP proving backend.
///
/// This is not a consumer backend-selection API. It supports the accepted buffered request family
/// and the native fixed/chunked upload plus streamed-response proving path.
#[cfg(feature = "native")]
pub fn native_http_engine(config: EngineConfig) -> Result<Engine, Error> {
    let factory = crate::backend::native_http_factory(&config);
    Engine::with_spawned_factory(config, factory)
}

/// Creates a private Rust-native HTTP proving Engine with an injected DNS nameserver.
///
/// This is a deterministic WP8 laboratory seam, not system resolver configuration and not a
/// consumer backend-selection API.
#[cfg(feature = "native")]
pub fn native_http_engine_with_nameserver(
    config: EngineConfig,
    nameserver: std::net::SocketAddr,
) -> Result<Engine, Error> {
    let factory = crate::backend::native_http_factory_with_nameserver(&config, nameserver);
    Engine::with_spawned_factory(config, factory)
}

/// Creates a private Rust-native HTTP proving Engine using the host's DNS configuration.
///
/// This remains a WP8 system-integration seam rather than public backend selection.
#[cfg(feature = "native")]
pub fn native_http_engine_with_system_dns(config: EngineConfig) -> Result<Engine, Error> {
    let factory = crate::backend::native_http_factory_with_system_dns(&config)?;
    Engine::with_spawned_factory(config, factory)
}

/// Creates a private Rust-native HTTPS proving Engine using platform trust.
///
/// The nameserver is injected for deterministic DNS ownership tests. This remains a WP8 proving
/// seam rather than public resolver or backend configuration.
#[cfg(feature = "native")]
pub fn native_https_engine_with_nameserver(
    config: EngineConfig,
    nameserver: std::net::SocketAddr,
) -> Result<Engine, Error> {
    let factory = crate::backend::native_https_factory_with_nameserver(&config, nameserver)?;
    Engine::with_spawned_factory(config, factory)
}

/// Creates a private Rust-native HTTPS proving Engine using host DNS and platform trust.
///
/// Ordinary `Engine::new` still does not select the native backend.
#[cfg(feature = "native")]
pub fn native_https_engine_with_system_dns(config: EngineConfig) -> Result<Engine, Error> {
    let factory = crate::backend::native_https_factory_with_system_dns(&config)?;
    Engine::with_spawned_factory(config, factory)
}

/// Creates a private Rust-native HTTPS proving Engine with one DER-encoded test trust root.
///
/// This modifies no operating-system trust store and is available only through test support.
#[cfg(feature = "native")]
pub fn native_https_engine_with_nameserver_and_test_root(
    config: EngineConfig,
    nameserver: std::net::SocketAddr,
    root_der: Vec<u8>,
) -> Result<Engine, Error> {
    let factory = crate::backend::native_https_factory_with_nameserver_and_test_root(
        &config, nameserver, root_der,
    )?;
    Engine::with_spawned_factory(config, factory)
}

/// Creates a manually driven private Rust-native HTTPS proving Engine.
///
/// DNS still runs on its Engine-owned resolver thread; the caller owns HTTP and TLS progress by
/// calling [`Engine::drive`] or [`Engine::drive_until`]. This is test support, not public backend
/// selection.
#[cfg(feature = "native")]
pub fn native_https_manual_engine_with_nameserver_and_test_root(
    config: EngineConfig,
    nameserver: std::net::SocketAddr,
    root_der: Vec<u8>,
) -> Result<Engine, Error> {
    if config.run_mode() != crate::RunMode::Manual {
        return Err(Error::new(
            crate::ErrorKind::WrongMode,
            "the manual native HTTPS proving constructor requires manual Engine mode",
        ));
    }
    let backend = crate::backend::native_https_backend_with_nameserver_and_test_root(
        &config, nameserver, root_der,
    )?;
    Engine::with_backend(config, backend)
}

#[cfg(all(test, feature = "curl-pilot"))]
pub(crate) fn curl_engine_with_test_ca(
    config: EngineConfig,
    ca_pem: Vec<u8>,
) -> Result<Engine, Error> {
    Engine::with_curl_test_ca(config, ca_pem)
}
