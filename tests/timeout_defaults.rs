//! Public timeout defaults and the existing ways to replace them.
//!
//! These assertions are deliberately independent of wall-clock scheduling: the
//! runtime deadline and backpressure paths have controlled-fixture coverage in
//! the HTTP, DNS, and TCP suites.

use std::net::SocketAddr;
use std::time::Duration;

#[cfg(feature = "resolver")]
use nbreq::ResolveRequest;
use nbreq::{Request, RequestOptions, StreamRequest, TcpConnectRequest, TlsOptions};

fn seconds(value: u64) -> Option<Duration> {
    Some(Duration::from_secs(value))
}

#[test]
fn buffered_http_options_and_request_builders_have_finite_defaults() {
    let options = RequestOptions::default();
    assert_eq!(options.connect_timeout, seconds(10));
    assert_eq!(options.inactivity_timeout, seconds(30));
    assert_eq!(options.total_timeout, seconds(120));

    for request in [
        Request::get("https://example.test/")
            .build()
            .expect("valid GET request"),
        Request::post("https://example.test/")
            .build()
            .expect("valid POST request"),
    ] {
        assert_eq!(request.options(), &options);
    }
}

#[test]
fn buffered_http_explicit_options_replace_defaults_without_reintroducing_timers() {
    let mut disabled = RequestOptions::default();
    disabled.connect_timeout = None;
    disabled.inactivity_timeout = None;
    disabled.total_timeout = None;
    let request = Request::get("https://example.test/")
        .options(disabled.clone())
        .build()
        .expect("valid request with explicit options");
    assert_eq!(request.options(), &disabled);

    let overridden = Request::get("https://example.test/")
        .connect_timeout(Duration::from_secs(2))
        .inactivity_timeout(Duration::from_secs(3))
        .total_timeout(Duration::from_secs(4))
        .build()
        .expect("valid request with timeout overrides");
    assert_eq!(overridden.options().connect_timeout, seconds(2));
    assert_eq!(overridden.options().inactivity_timeout, seconds(3));
    assert_eq!(overridden.options().total_timeout, seconds(4));
}

#[test]
fn http_fluent_timeouts_accept_some_and_none_with_last_setting_winning() {
    let request = Request::get("https://example.test/")
        .connect_timeout(Some(Duration::from_secs(2)))
        .connect_timeout(None)
        .inactivity_timeout(None)
        .inactivity_timeout(Some(Duration::from_secs(3)))
        .total_timeout(Some(Duration::from_secs(4)))
        .total_timeout(None)
        .build()
        .expect("valid request with optional fluent timeouts");
    assert_eq!(request.options().connect_timeout, None);
    assert_eq!(request.options().inactivity_timeout, seconds(3));
    assert_eq!(request.options().total_timeout, None);

    let stream = StreamRequest::get("https://example.test/events")
        .connect_timeout(Some(Duration::from_secs(2)))
        .connect_timeout(None)
        .inactivity_timeout(None)
        .total_timeout(Some(Duration::from_secs(4)))
        .total_timeout(None)
        .build()
        .expect("valid stream request with optional fluent timeouts");
    assert_eq!(stream.options().connect_timeout, None);
    assert_eq!(stream.options().inactivity_timeout, None);
    assert_eq!(stream.options().total_timeout, None);
}

#[test]
#[cfg(feature = "native")]
fn engine_bound_http_sugar_accepts_optional_timeout_setters() {
    use nbreq::{Engine, EngineConfig, ErrorKind, ExecuteError};

    let engine = Engine::new(EngineConfig::manual()).expect("manual Engine constructs");
    let result = engine
        .get("https://example.test/")
        .connect_timeout(Some(Duration::from_secs(2)))
        .connect_timeout(None)
        .inactivity_timeout(Some(Duration::from_secs(3)))
        .inactivity_timeout(None)
        .total_timeout(Some(Duration::from_secs(4)))
        .total_timeout(None)
        .call();
    assert!(matches!(
        result,
        Err(ExecuteError::Submission(error)) if error.kind() == ErrorKind::WrongMode
    ));
    engine.shutdown().expect("idle Engine shuts down");
}

#[test]
fn streaming_http_has_no_total_deadline_unless_selected() {
    let stream = StreamRequest::get("https://example.test/events")
        .build()
        .expect("valid streaming request");
    assert_eq!(stream.options().connect_timeout, seconds(10));
    assert_eq!(stream.options().inactivity_timeout, seconds(30));
    assert_eq!(stream.options().total_timeout, None);

    let explicit = StreamRequest::get("https://example.test/events")
        .total_timeout(Duration::from_secs(7))
        .build()
        .expect("valid streaming request with total timeout");
    assert_eq!(explicit.options().total_timeout, seconds(7));

    // Conversion must preserve caller-selected buffered options rather than
    // silently switch a request to the streaming builder's defaults.
    let buffered = Request::get("https://example.test/events")
        .build()
        .expect("valid buffered request for conversion");
    let converted = StreamRequest::from(buffered);
    assert_eq!(converted.options().total_timeout, seconds(120));
}

#[test]
#[cfg(feature = "resolver")]
fn public_dns_has_a_finite_total_deadline_that_can_be_replaced() {
    let default = ResolveRequest::hostname("example.test")
        .build()
        .expect("valid DNS request");
    assert_eq!(default.total_timeout(), seconds(30));

    let overridden = ResolveRequest::hostname("example.test")
        .total_timeout(Duration::from_secs(8))
        .build()
        .expect("valid DNS request with timeout override");
    assert_eq!(overridden.total_timeout(), seconds(8));

    let disabled = ResolveRequest::hostname("example.test")
        .total_timeout(Some(Duration::from_secs(8)))
        .total_timeout(None)
        .build()
        .expect("valid DNS request without timeout");
    assert_eq!(disabled.total_timeout(), None);
}

#[test]
fn plain_tcp_bounds_establishment_and_pending_writes_but_not_idle_reads() {
    let literal: SocketAddr = "127.0.0.1:443"
        .parse()
        .expect("valid loopback socket address");
    for request in [
        TcpConnectRequest::literal(literal)
            .build()
            .expect("valid literal TCP request"),
        TcpConnectRequest::hostname("example.test", 443)
            .build()
            .expect("valid hostname TCP request"),
    ] {
        assert_eq!(request.connect_timeout(), seconds(10));
        assert_eq!(request.read_inactivity_timeout(), None);
        assert_eq!(request.write_inactivity_timeout(), seconds(30));
    }

    let overridden = TcpConnectRequest::literal(literal)
        .connect_timeout(Duration::from_secs(2))
        .read_inactivity_timeout(Duration::from_secs(3))
        .write_inactivity_timeout(Duration::from_secs(4))
        .build()
        .expect("valid TCP request with timeout overrides");
    assert_eq!(overridden.connect_timeout(), seconds(2));
    assert_eq!(overridden.read_inactivity_timeout(), seconds(3));
    assert_eq!(overridden.write_inactivity_timeout(), seconds(4));

    let disabled = TcpConnectRequest::literal(literal)
        .connect_timeout(Some(Duration::from_secs(2)))
        .connect_timeout(None)
        .read_inactivity_timeout(Some(Duration::from_secs(3)))
        .read_inactivity_timeout(None)
        .write_inactivity_timeout(Some(Duration::from_secs(4)))
        .write_inactivity_timeout(None)
        .build()
        .expect("valid TCP request without timeouts");
    assert_eq!(disabled.connect_timeout(), None);
    assert_eq!(disabled.read_inactivity_timeout(), None);
    assert_eq!(disabled.write_inactivity_timeout(), None);
}

#[test]
fn standalone_tls_handshake_retains_its_ten_second_bound() {
    let options = TlsOptions::new("example.test").expect("valid TLS identity");
    assert_eq!(options.timeout(), Duration::from_secs(10));

    // Direct TLS establishment is bounded by the earlier of the TCP connect
    // and TLS handshake deadlines. A caller can remove the TCP bound while
    // retaining a longer explicit TLS handshake deadline.
    let long_tls = TlsOptions::new("example.test")
        .expect("valid TLS identity for longer handshake")
        .handshake_timeout(Duration::from_secs(600));
    let target: SocketAddr = "127.0.0.1:443"
        .parse()
        .expect("valid loopback socket address");
    let default_connect = TcpConnectRequest::literal(target)
        .build()
        .expect("valid default TCP request");
    let unbounded_connect = TcpConnectRequest::literal(target)
        .connect_timeout(None)
        .build()
        .expect("valid TCP request without connect timeout");
    assert_eq!(default_connect.connect_timeout(), seconds(10));
    assert_eq!(unbounded_connect.connect_timeout(), None);
    assert_eq!(long_tls.timeout(), Duration::from_secs(600));
}
