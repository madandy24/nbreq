//! Public hostname-to-address resolver contract.
//!
//! [`Resolver`] is an Engine-issued capability ticket. In this F0 skeleton, resolve operations
//! fail [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported) before identity allocation,
//! admission, callback reservation, or command queuing. They are not connected to the private DNS
//! owner.

use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::registry::Shared;
use crate::{Error, ErrorKind, ExecuteError, LimitKind, RequestId};

/// Maximum DNS presentation length without a terminal dot.
const MAX_DNS_NAME_LEN: usize = 253;
/// Maximum length of one DNS label.
const MAX_DNS_LABEL_LEN: usize = 63;

/// Selects which address families a public resolution collects.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum AddressFamily {
    /// Collect A and AAAA results.
    ///
    /// This is a dual-family collection, not HTTP's private A-first / AAAA-on-NoData fallback. A
    /// transport or protocol failure in either required family fails the operation rather than
    /// returning a partial set.
    #[default]
    Both,
    /// Collect only A records.
    Ipv4,
    /// Collect only AAAA records.
    Ipv6,
}

/// Combined-family ordering of a collected public resolution.
///
/// Order within each family is preserved. Combined IPv4/IPv6 ordering does not promise parallel
/// queries or Happy Eyeballs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum AddressOrder {
    /// Emit IPv4 addresses before IPv6 addresses.
    #[default]
    Ipv4ThenIpv6,
    /// Emit IPv6 addresses before IPv4 addresses.
    Ipv6ThenIpv4,
}

/// Cache behaviour for one public resolution.
///
/// Public resolutions share the Engine-owned DNS cache with HTTP lookups. A public
/// [`CacheMode::Refresh`] can therefore replace an entry later observed by HTTP on that Engine.
/// Search-suffix policy never affects HTTP.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum CacheMode {
    /// Read a still-valid cache entry if present, and populate the cache from the network result.
    #[default]
    Use,
    /// Skip the cache read and replace the cache entry from the network.
    Refresh,
    /// Neither read nor populate the cache.
    Bypass,
}

/// Outcome of a completed DNS exchange, including valid negative answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResolveStatus {
    /// The name has one or more addresses.
    Answer,
    /// The name does not exist (NXDOMAIN).
    NameNotFound,
    /// The name exists but the requested family has no address data (NoData).
    NoData,
}

/// One address collected by a public resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedAddress {
    address: IpAddr,
}

impl ResolvedAddress {
    #[allow(dead_code)] // Constructed when Resolver wiring begins.
    pub(crate) fn new(address: IpAddr) -> Self {
        Self { address }
    }

    /// Returns the collected IP address.
    #[must_use]
    pub fn address(&self) -> IpAddr {
        self.address
    }
}

/// A completed public resolution, including valid NXDOMAIN and NoData answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveResponse {
    name: String,
    status: ResolveStatus,
    addresses: Vec<ResolvedAddress>,
    valid_until: Option<Instant>,
    from_cache: bool,
}

impl ResolveResponse {
    #[allow(dead_code)] // Constructed when Resolver wiring begins.
    pub(crate) fn new(
        name: String,
        status: ResolveStatus,
        addresses: Vec<ResolvedAddress>,
        valid_until: Option<Instant>,
        from_cache: bool,
    ) -> Self {
        Self {
            name,
            status,
            addresses,
            valid_until,
            from_cache,
        }
    }

    /// Returns the normalized lookup identity: lowercase ASCII without a terminal dot.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns whether this completed exchange is an answer or a valid negative result.
    #[must_use]
    pub fn status(&self) -> ResolveStatus {
        self.status
    }

    /// Returns the collected addresses in the requested family order.
    ///
    /// Empty for [`ResolveStatus::NameNotFound`] and [`ResolveStatus::NoData`].
    #[must_use]
    pub fn addresses(&self) -> &[ResolvedAddress] {
        &self.addresses
    }

    /// Returns in-process cache validity, or `None` when the result is not cached.
    ///
    /// This is an [`Instant`], not a wall-clock timestamp.
    #[must_use]
    pub fn valid_until(&self) -> Option<Instant> {
        self.valid_until
    }

    /// Returns whether this result was served from the Engine-owned cache.
    #[must_use]
    pub fn from_cache(&self) -> bool {
        self.from_cache
    }
}

/// Canonical terminal outcome of an accepted public resolution.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ResolveCompletion {
    /// A completed DNS exchange, including NXDOMAIN and NoData.
    Completed(ResolveResponse),
    /// A transport, protocol, timeout, or policy failure.
    Failed(Error),
    /// Explicit cancellation won the terminal-state race.
    Cancelled,
}

/// Result of a waiter-local public-resolution timeout.
#[derive(Debug)]
#[non_exhaustive]
pub enum ResolveWaitOutcome {
    /// The resolution reached its canonical terminal outcome.
    Completed(ResolveCompletion),
    /// The local wait expired; the resolution and its cancellation handle remain live.
    TimedOut(PendingResolve),
}

/// An owned public hostname-resolution request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveRequest {
    name: String,
    absolute: bool,
    address_family: AddressFamily,
    address_order: AddressOrder,
    cache_mode: CacheMode,
    max_results: Option<usize>,
    use_search_suffixes: bool,
    total_timeout: Option<Duration>,
}

impl ResolveRequest {
    /// Starts a resolution builder for an exact ASCII or punycode hostname.
    #[must_use]
    pub fn hostname(name: impl Into<String>) -> ResolveRequestBuilder {
        ResolveRequestBuilder {
            name: name.into(),
            address_family: AddressFamily::Both,
            address_order: AddressOrder::Ipv4ThenIpv6,
            cache_mode: CacheMode::Use,
            max_results: None,
            use_search_suffixes: false,
            total_timeout: None,
        }
    }

    /// Returns the normalized lookup identity.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns whether the caller supplied an explicit absolute spelling (one terminal dot).
    ///
    /// An absolute name never receives search-suffix expansion, even when
    /// [`Self::use_search_suffixes`] is true.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        self.absolute
    }

    /// Returns the requested address family.
    #[must_use]
    pub fn address_family(&self) -> AddressFamily {
        self.address_family
    }

    /// Returns the combined-family result order.
    #[must_use]
    pub fn address_order(&self) -> AddressOrder {
        self.address_order
    }

    /// Returns the requested cache mode.
    #[must_use]
    pub fn cache_mode(&self) -> CacheMode {
        self.cache_mode
    }

    /// Returns the per-request result cap when one was selected.
    ///
    /// `None` means the Engine ceiling is used at admission.
    #[must_use]
    pub fn max_results(&self) -> Option<usize> {
        self.max_results
    }

    /// Returns whether system-search suffix expansion was requested.
    ///
    /// Suffix expansion is per-request opt-in and is not implemented in this F0 skeleton. A
    /// trailing-dot absolute spelling suppresses expansion even when this flag is true; see
    /// [`Self::applies_search_suffixes`].
    #[must_use]
    pub fn use_search_suffixes(&self) -> bool {
        self.use_search_suffixes
    }

    /// Returns whether F1 may apply system-search suffix expansion for this request.
    ///
    /// True only when search was requested and the name is not an explicit absolute spelling.
    #[must_use]
    pub fn applies_search_suffixes(&self) -> bool {
        self.use_search_suffixes && !self.absolute
    }

    /// Returns the total resolution deadline beginning at admission.
    ///
    /// Expiry is classified as [`TimeoutKind::Total`](crate::TimeoutKind::Total).
    #[must_use]
    pub fn total_timeout(&self) -> Option<Duration> {
        self.total_timeout
    }
}

/// Builder for an owned [`ResolveRequest`].
#[derive(Clone, Debug)]
pub struct ResolveRequestBuilder {
    name: String,
    address_family: AddressFamily,
    address_order: AddressOrder,
    cache_mode: CacheMode,
    max_results: Option<usize>,
    use_search_suffixes: bool,
    total_timeout: Option<Duration>,
}

impl ResolveRequestBuilder {
    /// Selects which address families to collect.
    #[must_use]
    pub fn address_family(mut self, family: AddressFamily) -> Self {
        self.address_family = family;
        self
    }

    /// Selects combined-family result order.
    #[must_use]
    pub fn address_order(mut self, order: AddressOrder) -> Self {
        self.address_order = order;
        self
    }

    /// Selects cache read/populate behaviour.
    #[must_use]
    pub fn cache_mode(mut self, mode: CacheMode) -> Self {
        self.cache_mode = mode;
        self
    }

    /// Selects a per-request result cap at or below the Engine ceiling.
    ///
    /// Zero is rejected at build time. A value above the Engine ceiling is rejected at start,
    /// submit, or execute without allocating an ID or permit.
    #[must_use]
    pub fn max_results(mut self, max_results: usize) -> Self {
        self.max_results = Some(max_results);
        self
    }

    /// Requests per-request system-search suffix expansion.
    ///
    /// This never becomes an Engine-wide switch and cannot affect HTTP lookups. A trailing-dot
    /// absolute spelling still suppresses expansion. F0 stores the flag and does not expand
    /// suffixes.
    #[must_use]
    pub fn use_search_suffixes(mut self, enabled: bool) -> Self {
        self.use_search_suffixes = enabled;
        self
    }

    /// Sets the maximum total duration beginning at resolution acceptance.
    #[must_use]
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        self.total_timeout = Some(timeout);
        self
    }

    /// Validates the name and result bound, then returns the request.
    pub fn build(self) -> Result<ResolveRequest, Error> {
        let normalized = normalize_dns_name(&self.name)?;
        if let Some(0) = self.max_results {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "resolution max_results must be greater than zero",
            ));
        }
        Ok(ResolveRequest {
            name: normalized.identity,
            absolute: normalized.absolute,
            address_family: self.address_family,
            address_order: self.address_order,
            cache_mode: self.cache_mode,
            max_results: self.max_results,
            use_search_suffixes: self.use_search_suffixes,
            total_timeout: self.total_timeout,
        })
    }
}

/// Cheap cloneable hostname-resolution handle issued by an [`Engine`](crate::Engine).
///
/// Resolver has no public constructor. It does not own or extend Engine lifetime. Detached
/// handles reject new work with [`ErrorKind::EngineStopped`].
///
/// ```compile_fail
/// let _ = nbreq::Resolver::new();
/// ```
#[derive(Clone, Debug)]
pub struct Resolver {
    shared: Arc<Shared>,
    max_resolve_results: NonZeroUsize,
}

impl Resolver {
    pub(crate) fn new(shared: Arc<Shared>, max_resolve_results: NonZeroUsize) -> Self {
        Self {
            shared,
            max_resolve_results,
        }
    }

    /// Starts a callback-oriented resolution.
    ///
    /// The callback is queued only after canonical terminal state is committed and never runs on
    /// the resolver thread or while internal locks are held. Rejected starts drop the callback
    /// without invoking it.
    pub fn start<F>(&self, request: ResolveRequest, callback: F) -> Result<ResolveHandle, Error>
    where
        F: FnOnce(ResolveCompletion) + Send + 'static,
    {
        self.reject_resolution(request)?;
        drop(callback);
        Err(self.unavailable())
    }

    /// Submits a resolution and returns its direct terminal-state waiter.
    pub fn submit(&self, request: ResolveRequest) -> Result<PendingResolve, Error> {
        self.reject_resolution(request)?;
        Err(self.unavailable())
    }

    /// Submits a resolution and blocks on its direct terminal-state waiter.
    pub fn execute(&self, request: ResolveRequest) -> Result<ResolveResponse, ExecuteError> {
        match self.submit(request) {
            Ok(pending) => match pending.wait() {
                ResolveCompletion::Completed(response) => Ok(response),
                ResolveCompletion::Failed(error) => Err(ExecuteError::Failed(error)),
                ResolveCompletion::Cancelled => Err(ExecuteError::Cancelled),
            },
            Err(error) => Err(ExecuteError::Submission(error)),
        }
    }

    /// Cancels an Engine-scoped resolution ID.
    ///
    /// Public resolution is not wired in F0, so this rejects before touching the HTTP registry.
    pub fn cancel(&self, _request_id: RequestId) -> Result<(), Error> {
        Err(self.stopped_or_unavailable())
    }

    fn reject_resolution(&self, request: ResolveRequest) -> Result<(), Error> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(stopped_error());
        }
        if let Some(max_results) = request.max_results {
            if max_results > self.max_resolve_results.get() {
                return Err(Error::limit(
                    LimitKind::ResolveResults,
                    format!(
                        "resolution max_results exceeds the Engine ceiling of {}",
                        self.max_resolve_results
                    ),
                ));
            }
        }
        Ok(())
    }

    fn unavailable(&self) -> Error {
        Error::new(
            ErrorKind::Unsupported,
            "public hostname resolution is not available yet",
        )
    }

    fn stopped_or_unavailable(&self) -> Error {
        if self.shared.stopped.load(Ordering::Acquire) {
            stopped_error()
        } else {
            self.unavailable()
        }
    }
}

/// Engine-bound control handle for one accepted public resolution.
#[derive(Clone, Debug)]
pub struct ResolveHandle {
    resolver: Resolver,
    id: RequestId,
}

impl ResolveHandle {
    #[allow(dead_code)] // Constructed when Resolver wiring begins.
    pub(crate) fn new(resolver: Resolver, id: RequestId) -> Self {
        Self { resolver, id }
    }

    /// Returns the opaque Engine-scoped identity.
    #[must_use]
    pub fn id(&self) -> RequestId {
        self.id
    }

    /// Requests cancellation. Repeated and post-terminal calls are harmless once wired.
    pub fn cancel(&self) -> Result<(), Error> {
        self.resolver.cancel(self.id)
    }
}

/// Accepted public resolution plus a direct terminal-state waiter.
#[derive(Debug)]
pub struct PendingResolve {
    handle: ResolveHandle,
}

impl PendingResolve {
    #[allow(dead_code)] // Constructed when Resolver wiring begins.
    pub(crate) fn new(handle: ResolveHandle) -> Self {
        Self { handle }
    }

    /// Returns a clone of the independent cancellation handle.
    #[must_use]
    pub fn handle(&self) -> ResolveHandle {
        self.handle.clone()
    }

    /// Returns whether canonical terminal state has been committed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        false
    }

    pub(crate) fn try_completion(&self) -> Option<ResolveCompletion> {
        None
    }

    pub(crate) fn issued_engine_id(&self) -> u64 {
        self.handle.id.engine
    }

    /// Waits for and returns the canonical terminal outcome.
    #[must_use]
    pub fn wait(self) -> ResolveCompletion {
        let _ = self;
        ResolveCompletion::Failed(Error::new(
            ErrorKind::Unsupported,
            "public hostname resolution is not available yet",
        ))
    }

    /// Waits locally without changing resolution state or cancelling on timeout.
    #[must_use]
    pub fn wait_for(self, _duration: Duration) -> ResolveWaitOutcome {
        ResolveWaitOutcome::TimedOut(self)
    }
}

pub(crate) struct NormalizedDnsName {
    pub(crate) identity: String,
    pub(crate) absolute: bool,
}

pub(crate) fn normalize_dns_name(raw: &str) -> Result<NormalizedDnsName, Error> {
    if raw.is_empty() {
        return Err(invalid_dns_name("DNS name is empty"));
    }
    if !raw.is_ascii() {
        return Err(invalid_dns_name(
            "DNS names must be exact ASCII or punycode; Unicode is not converted",
        ));
    }
    let absolute = raw.ends_with('.');
    let without_dot = raw.strip_suffix('.').unwrap_or(raw);
    if without_dot.is_empty() {
        return Err(invalid_dns_name("the DNS root name is not allowed"));
    }
    if without_dot.len() > MAX_DNS_NAME_LEN {
        return Err(invalid_dns_name(
            "DNS name exceeds the 253-octet presentation limit",
        ));
    }

    let mut identity = String::with_capacity(without_dot.len());
    for (index, label) in without_dot.split('.').enumerate() {
        validate_dns_label(label)?;
        if index > 0 {
            identity.push('.');
        }
        identity.extend(label.chars().map(|ch| ch.to_ascii_lowercase()));
    }
    Ok(NormalizedDnsName { identity, absolute })
}

fn validate_dns_label(label: &str) -> Result<(), Error> {
    if label.is_empty() {
        return Err(invalid_dns_name("DNS name contains an empty label"));
    }
    if label.len() > MAX_DNS_LABEL_LEN {
        return Err(invalid_dns_name("DNS label exceeds the 63-octet limit"));
    }
    let bytes = label.as_bytes();
    if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return Err(invalid_dns_name(
            "DNS label cannot begin or end with a hyphen",
        ));
    }
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    {
        return Err(invalid_dns_name(
            "DNS names must be exact ASCII letters, digits, hyphens, and dots",
        ));
    }
    Ok(())
}

fn invalid_dns_name(message: &str) -> Error {
    Error::new(ErrorKind::InvalidRequest, message)
}

fn stopped_error() -> Error {
    Error::new(
        ErrorKind::EngineStopped,
        "the owning Engine has stopped accepting work",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, EngineConfig};
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

    fn built(name: &str) -> ResolveRequest {
        ResolveRequest::hostname(name)
            .build()
            .unwrap_or_else(|error| panic!("{name:?} must build: {error}"))
    }

    #[test]
    fn builder_defaults_are_both_ipv4_then_ipv6_use_cache_and_no_search() {
        let request = built("Example.COM.");
        assert_eq!(request.name(), "example.com");
        assert!(request.is_absolute());
        assert_eq!(request.address_family(), AddressFamily::Both);
        assert_eq!(request.address_order(), AddressOrder::Ipv4ThenIpv6);
        assert_eq!(request.cache_mode(), CacheMode::Use);
        assert_eq!(request.max_results(), None);
        assert!(!request.use_search_suffixes());
        assert_eq!(request.total_timeout(), None);
    }

    #[test]
    fn builder_accepts_zero_or_one_terminal_dot_and_lowercases_identity() {
        assert_eq!(built("localhost").name(), "localhost");
        assert!(!built("localhost").is_absolute());
        assert_eq!(built("localhost.").name(), "localhost");
        assert!(built("localhost.").is_absolute());
        assert_eq!(
            built("XN--NXASMQ2B.example.").name(),
            "xn--nxasmq2b.example"
        );
    }

    #[test]
    fn builder_rejects_root_empty_malformed_and_overlong_names() {
        let long_label = "a".repeat(64);
        let long_name = format!("{}.{}", "a".repeat(63), "a".repeat(190));
        for name in [
            "",
            ".",
            "..",
            ".example.com",
            "example..com",
            "example.com..",
            "-example.com",
            "example-.com",
            "exam_ple.com",
            "ex ample.com",
            "münchen.de",
            long_label.as_str(),
            long_name.as_str(),
        ] {
            let error = ResolveRequest::hostname(name)
                .build()
                .expect_err("invalid DNS name must fail during construction");
            assert_eq!(error.kind(), ErrorKind::InvalidRequest, "{name:?}");
        }
        ResolveRequest::hostname("example.com.")
            .build()
            .expect("one terminal dot is an absolute spelling");
        let error = ResolveRequest::hostname("example.com")
            .max_results(0)
            .build()
            .expect_err("zero max_results must fail during construction");
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
    }

    #[test]
    fn builder_stores_search_opt_in_without_expanding_suffixes() {
        let request = ResolveRequest::hostname("www")
            .use_search_suffixes(true)
            .cache_mode(CacheMode::Bypass)
            .max_results(4)
            .build()
            .expect("search opt-in must build");
        assert!(request.use_search_suffixes());
        assert!(!request.is_absolute());
        assert!(request.applies_search_suffixes());
        assert_eq!(request.name(), "www");
        assert_eq!(request.cache_mode(), CacheMode::Bypass);
        assert_eq!(request.max_results(), Some(4));

        let absolute = ResolveRequest::hostname("www.")
            .use_search_suffixes(true)
            .build()
            .expect("absolute search opt-in must build");
        assert!(absolute.use_search_suffixes());
        assert!(absolute.is_absolute());
        assert!(
            !absolute.applies_search_suffixes(),
            "a trailing-dot spelling must suppress suffix expansion"
        );
    }

    #[test]
    fn unavailable_operations_reject_before_admission_and_do_not_run_callbacks() {
        let engine = Engine::with_backend(EngineConfig::spawned(), crate::backend::scaffold())
            .expect("scaffold Engine must construct");
        let resolver = engine.resolver();
        let clone = resolver.clone();
        let request = built("example.com");
        let before = engine.metrics();

        let ran = StdArc::new(AtomicBool::new(false));
        let flag = StdArc::clone(&ran);
        let start_error = resolver
            .start(request.clone(), move |_| {
                flag.store(true, AtomicOrdering::SeqCst);
            })
            .expect_err("F0 resolution must be unavailable");
        assert_eq!(start_error.kind(), ErrorKind::Unsupported);
        assert!(!ran.load(AtomicOrdering::SeqCst));

        let submit_error = clone
            .submit(request.clone())
            .expect_err("F0 submit must be unavailable");
        assert_eq!(submit_error.kind(), ErrorKind::Unsupported);

        let execute_error = resolver
            .execute(request)
            .expect_err("F0 execute must be unavailable");
        match execute_error {
            ExecuteError::Submission(error) => assert_eq!(error.kind(), ErrorKind::Unsupported),
            other => panic!("execute must fail before acceptance: {other:?}"),
        }

        let after = engine.metrics();
        assert_eq!(before, after);
        assert_eq!(after.resolutions_accepted(), 0);
        assert_eq!(after.current().inflight_resolutions(), 0);
        engine.shutdown().expect("scaffold Engine must stop");
    }

    #[test]
    fn per_request_max_results_cannot_exceed_the_engine_ceiling() {
        let ceiling = NonZeroUsize::new(2).expect("two is non-zero");
        let engine = Engine::with_backend(
            EngineConfig::spawned().with_max_resolve_results(ceiling),
            crate::backend::scaffold(),
        )
        .expect("bounded scaffold Engine must construct");
        let request = ResolveRequest::hostname("example.com")
            .max_results(3)
            .build()
            .expect("over-ceiling request must still build");
        let error = engine
            .resolver()
            .submit(request)
            .expect_err("over-ceiling max_results must fail before admission");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.limit_kind(), Some(LimitKind::ResolveResults));
        assert_eq!(engine.metrics().resolutions_accepted(), 0);
        engine.shutdown().expect("bounded Engine must stop");
    }

    #[test]
    fn detached_resolver_rejects_with_engine_stopped() {
        let engine = Engine::with_backend(EngineConfig::spawned(), crate::backend::scaffold())
            .expect("scaffold Engine must construct");
        let resolver = engine.resolver();
        engine.shutdown().expect("scaffold Engine must stop");
        let error = resolver
            .submit(built("example.com"))
            .expect_err("detached resolver must observe Engine stop");
        assert_eq!(error.kind(), ErrorKind::EngineStopped);
    }
}
