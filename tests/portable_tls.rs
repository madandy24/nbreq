#![cfg(feature = "native")]

// Uses only ordinary public NBReq configuration; no test-support verifier or OS trust edits.
#[path = "support/portable_tls_fixture.rs"]
mod fixture;

use std::thread;
use std::time::{Duration, Instant};

use fixture::{Identity, Invalid, Peer, Protocol, Service};
use nbreq::{
    Completion, Engine, EngineConfig, TcpConnectCompletion, TcpConnectRequest, TcpRead,
    TlsConnectCompletion, TlsFailure, TlsOptions, TlsTrust,
};

fn config(manual: bool, root: &[u8]) -> EngineConfig {
    let config = if manual {
        EngineConfig::manual()
    } else {
        EngineConfig::spawned()
    };
    config
        .with_tls_trust(TlsTrust::SuppliedRootsOnly)
        .with_additional_tls_root_certificate(root.to_vec())
}

fn progress(engine: &mut Engine, manual: bool, deadline: Instant) {
    assert!(
        Instant::now() < deadline,
        "bounded application I/O timed out"
    );
    if manual {
        engine
            .drive(Instant::now() + Duration::from_millis(10))
            .expect("manual progress");
    } else {
        thread::sleep(Duration::from_millis(1));
    }
}

fn https(engine: &mut Engine, manual: bool, peer: &Peer) -> Completion {
    let pending = engine
        .client()
        .submit(
            nbreq::Request::get(format!("https://{}/", peer.address))
                .total_timeout(Duration::from_secs(5))
                .build()
                .expect("HTTPS request"),
        )
        .expect("submit HTTPS");
    if manual {
        engine.drive_until(pending).expect("manual HTTPS drive")
    } else {
        pending.wait()
    }
}

fn assert_http_ok(result: Completion) {
    match result {
        Completion::Completed(response) => {
            assert_eq!(response.status(), 200);
            assert_eq!(response.body(), b"ok");
        }
        other => panic!("trusted HTTPS must succeed: {other:?}"),
    }
}

fn assert_http_rejected(result: Completion, expected: TlsFailure) {
    match result {
        Completion::Failed(error) => assert_eq!(error.tls_failure(), Some(expected), "{error:?}"),
        other => panic!("invalid HTTPS certificate must fail: {other:?}"),
    }
}

fn tls(
    engine: &mut Engine,
    manual: bool,
    peer: &Peer,
    name: &str,
    upgrade: bool,
) -> TlsConnectCompletion {
    let request = TcpConnectRequest::literal(peer.address)
        .connect_timeout(Duration::from_secs(3))
        .read_inactivity_timeout(Duration::from_secs(4))
        .write_inactivity_timeout(Duration::from_secs(4))
        .build()
        .expect("TCP request");
    let options = TlsOptions::new(name)
        .expect("TLS identity")
        .handshake_timeout(Duration::from_secs(4));
    let pending = if upgrade {
        let pending = engine
            .tcp_connector()
            .submit(request)
            .expect("plain submit");
        let result = if manual {
            engine.drive_until(pending).expect("plain drive")
        } else {
            pending.wait()
        };
        let mut plain = match result {
            TcpConnectCompletion::Completed(connection) => connection,
            other => panic!("plain connection must succeed: {other:?}"),
        };
        plain
            .try_send(b"STARTTLS\r\n".to_vec())
            .expect("STARTTLS command");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n") {
            progress(engine, manual, deadline);
            let mut byte = [0];
            match plain.try_read(&mut byte).expect("STARTTLS ACK") {
                TcpRead::Pending => {}
                TcpRead::Data(1) => response.push(byte[0]),
                other => panic!("STARTTLS ACK incomplete: {other:?}"),
            }
            assert!(response.len() <= 64);
        }
        assert_eq!(response, b"220 Ready\r\n");
        plain.submit_tls(options).expect("upgrade submit")
    } else {
        engine
            .tcp_connector()
            .submit_tls(request, options)
            .expect("TLS submit")
    };
    if manual {
        engine.drive_until(pending).expect("TLS drive")
    } else {
        pending.wait()
    }
}

fn assert_tls_ok(engine: &mut Engine, manual: bool, result: TlsConnectCompletion) {
    let mut connection = match result {
        TlsConnectCompletion::Completed(connection) => connection,
        other => panic!("trusted TLS must succeed: {other:?}"),
    };
    connection
        .try_send(b"ping".to_vec())
        .expect("encrypted send");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut response = Vec::new();
    while response.len() < 4 {
        progress(engine, manual, deadline);
        let mut bytes = [0; 4];
        match connection.try_read(&mut bytes).expect("encrypted read") {
            TcpRead::Pending => {}
            TcpRead::Data(count) => response.extend_from_slice(&bytes[..count]),
            other => panic!("encrypted response incomplete: {other:?}"),
        }
    }
    assert_eq!(response, b"pong");
}

#[test]
fn trusted_https_uses_supplied_roots_in_both_run_modes_and_tls_versions() {
    let identity = Identity::new("127.0.0.1", Invalid::None);
    for (manual, protocol) in [(false, Protocol::Tls12), (true, Protocol::Tls13)] {
        let mut engine = Engine::new(config(manual, &identity.root)).expect("portable Engine");
        let peer = Peer::spawn(&identity, protocol, Service::Http(None));
        assert_http_ok(https(&mut engine, manual, &peer));
        peer.assert_completed(protocol);
        engine.shutdown().expect("Engine shutdown");
    }
}

#[test]
fn trusted_direct_tls_uses_supplied_roots_in_both_run_modes_and_tls_versions() {
    let identity = Identity::new("localhost", Invalid::None);
    for (manual, protocol) in [(true, Protocol::Tls12), (false, Protocol::Tls13)] {
        let mut engine = Engine::new(config(manual, &identity.root)).expect("portable Engine");
        let peer = Peer::spawn(&identity, protocol, Service::Direct);
        let result = tls(&mut engine, manual, &peer, "localhost", false);
        assert_tls_ok(&mut engine, manual, result);
        peer.assert_completed(protocol);
        engine.shutdown().expect("Engine shutdown");
    }
}

#[test]
fn trusted_starttls_uses_supplied_roots_in_both_run_modes_and_tls_versions() {
    let identity = Identity::new("127.0.0.1", Invalid::None);
    for (manual, protocol) in [(false, Protocol::Tls12), (true, Protocol::Tls13)] {
        let mut engine = Engine::new(config(manual, &identity.root)).expect("portable Engine");
        let peer = Peer::spawn(&identity, protocol, Service::StartTls);
        let result = tls(&mut engine, manual, &peer, "127.0.0.1", true);
        assert_tls_ok(&mut engine, manual, result);
        peer.assert_completed(protocol);
        engine.shutdown().expect("Engine shutdown");
    }
}

#[test]
fn portable_trust_rejects_empty_and_malformed_roots_before_engine_start() {
    for base in [EngineConfig::spawned(), EngineConfig::manual()] {
        for roots in [
            None,
            Some(Vec::new()),
            Some(vec![1, 2, 3]),
            Some(b"-----BEGIN CERTIFICATE-----".to_vec()),
        ] {
            let mut config = base.clone().with_tls_trust(TlsTrust::SuppliedRootsOnly);
            if let Some(root) = roots {
                config = config.with_additional_tls_root_certificate(root);
            }
            let error = Engine::new(config)
                .err()
                .expect("invalid roots must reject Engine construction");
            assert_eq!(error.tls_failure(), Some(TlsFailure::Configuration));
        }
    }
}

#[test]
fn supplied_roots_preserve_certificate_validity_purpose_and_signature_checks() {
    for (invalid, expected) in [
        (Invalid::Expired, TlsFailure::CertificateExpired),
        (Invalid::Future, TlsFailure::CertificateNotYetValid),
        (Invalid::ClientOnly, TlsFailure::CertificateInvalid),
        (Invalid::Signature, TlsFailure::CertificateInvalid),
    ] {
        let identity = Identity::new("127.0.0.1", invalid);
        let mut engine = Engine::new(config(false, &identity.root)).expect("portable Engine");
        let peer = Peer::spawn(&identity, Protocol::Tls13, Service::Http(None));
        assert_http_rejected(https(&mut engine, false, &peer), expected);
        engine.shutdown().expect("Engine shutdown");
    }
}

#[test]
fn supplied_roots_preserve_dns_and_ip_identity_checks_for_direct_and_starttls() {
    let identity = Identity::new("localhost", Invalid::None);
    for (name, upgrade) in [("wrong.test", false), ("127.0.0.1", true)] {
        let mut engine = Engine::new(config(false, &identity.root)).expect("portable Engine");
        let service = if upgrade {
            Service::StartTls
        } else {
            Service::Direct
        };
        let peer = Peer::spawn(&identity, Protocol::Tls13, service);
        match tls(&mut engine, false, &peer, name, upgrade) {
            TlsConnectCompletion::Failed(error) => assert_eq!(
                error.tls_failure(),
                Some(TlsFailure::CertificateHostnameMismatch)
            ),
            other => panic!("wrong identity must fail: {other:?}"),
        }
        engine.shutdown().expect("Engine shutdown");
    }
}

#[test]
fn supplied_root_sets_are_cloned_and_isolated_between_engines() {
    let first = Identity::new("127.0.0.1", Invalid::None);
    let second = Identity::new("127.0.0.1", Invalid::None);
    // Roots added both before and after choosing a source must survive that choice and cloning.
    let both = EngineConfig::spawned()
        .with_additional_tls_root_certificate(first.root.clone())
        .with_tls_trust(TlsTrust::SuppliedRootsOnly)
        .with_additional_tls_root_certificate(second.root.clone());
    for identity in [&first, &second] {
        let mut engine = Engine::new(both.clone()).expect("two roots Engine");
        let peer = Peer::spawn(identity, Protocol::Tls13, Service::Http(None));
        assert_http_ok(https(&mut engine, false, &peer));
        peer.assert_completed(Protocol::Tls13);
        engine.shutdown().expect("Engine shutdown");
    }
    let mut engine = Engine::new(config(false, &first.root)).expect("isolated Engine");
    let peer = Peer::spawn(&second, Protocol::Tls13, Service::Http(None));
    assert_http_rejected(
        https(&mut engine, false, &peer),
        TlsFailure::CertificateUnknownIssuer,
    );
    engine.shutdown().expect("Engine shutdown");
}

#[test]
fn redirects_keep_the_same_explicit_root_set() {
    let first = Identity::new("127.0.0.1", Invalid::None);
    let second = Identity::new("127.0.0.1", Invalid::None);
    for include_second in [true, false] {
        let mut config = config(true, &first.root);
        if include_second {
            config = config.with_additional_tls_root_certificate(second.root.clone());
        }
        let mut engine = Engine::new(config).expect("redirect Engine");
        let target = Peer::spawn(&second, Protocol::Tls13, Service::Http(None));
        let source = Peer::spawn(
            &first,
            Protocol::Tls13,
            Service::Http(Some(format!("https://{}/next", target.address))),
        );
        let result = https(&mut engine, true, &source);
        if include_second {
            assert_http_ok(result);
            target.assert_completed(Protocol::Tls13);
        } else {
            assert_http_rejected(result, TlsFailure::CertificateUnknownIssuer);
        }
        source.assert_completed(Protocol::Tls13);
        engine.shutdown().expect("Engine shutdown");
    }
}

#[test]
fn platform_is_still_the_default_and_selection_survives_clone() {
    for config in [EngineConfig::spawned(), EngineConfig::manual()] {
        let mode = config.run_mode();
        assert_eq!(config.tls_trust(), TlsTrust::Platform);
        let selected = config.with_tls_trust(TlsTrust::SuppliedRootsOnly);
        assert_eq!(selected.clone().tls_trust(), TlsTrust::SuppliedRootsOnly);
        assert_eq!(selected.run_mode(), mode);
    }
}

#[cfg(feature = "bundled-roots")]
#[test]
fn bundled_mozilla_roots_accept_additions_without_leaking_custom_trust() {
    let identity = Identity::new("127.0.0.1", Invalid::None);
    for include_custom in [true, false] {
        let mut config = EngineConfig::spawned().with_tls_trust(TlsTrust::BundledMozilla);
        if include_custom {
            config = config.with_additional_tls_root_certificate(identity.root.clone());
        }
        let mut engine = Engine::new(config).expect("bundled Engine");
        let peer = Peer::spawn(&identity, Protocol::Tls13, Service::Http(None));
        let result = https(&mut engine, false, &peer);
        if include_custom {
            assert_http_ok(result);
            peer.assert_completed(Protocol::Tls13);
        } else {
            assert_http_rejected(result, TlsFailure::CertificateUnknownIssuer);
        }
        engine.shutdown().expect("Engine shutdown");
    }
    let error = Engine::new(
        EngineConfig::spawned()
            .with_tls_trust(TlsTrust::BundledMozilla)
            .with_additional_tls_root_certificate(vec![1, 2, 3]),
    )
    .err()
    .expect("invalid extra root");
    assert_eq!(error.tls_failure(), Some(TlsFailure::Configuration));
}

#[cfg(not(feature = "bundled-roots"))]
#[test]
fn bundled_roots_without_the_feature_is_explicitly_unsupported() {
    let identity = Identity::new("127.0.0.1", Invalid::None);
    for config in [
        EngineConfig::spawned(),
        EngineConfig::manual().with_additional_tls_root_certificate(identity.root.clone()),
    ] {
        let error = Engine::new(config.with_tls_trust(TlsTrust::BundledMozilla))
            .err()
            .expect("missing root bundle must not fall back even with custom roots");
        assert_eq!(error.kind(), nbreq::ErrorKind::Unsupported);
    }
}
