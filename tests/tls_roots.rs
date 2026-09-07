#![cfg(feature = "native")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use nbreq::{Completion, Engine, EngineConfig, Request, TlsFailure};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

struct Identity {
    root: Vec<u8>,
    server: Arc<ServerConfig>,
}

impl Identity {
    fn new(host: &str, expired: bool) -> Self {
        let root_key = KeyPair::generate().expect("root key");
        let mut root_params = CertificateParams::new(Vec::<String>::new()).expect("root params");
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params
            .distinguished_name
            .push(DnType::CommonName, "NBReq private CA fixture");
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = root_params
            .self_signed(&root_key)
            .expect("root certificate");
        let key = KeyPair::generate().expect("leaf key");
        let mut params = CertificateParams::new(vec![host.to_owned()]).expect("leaf params");
        params.distinguished_name.push(DnType::CommonName, host);
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(30);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        if expired {
            params.not_before = date_time_ymd(2010, 1, 1);
            params.not_after = date_time_ymd(2011, 1, 1);
        }
        let leaf = params
            .signed_by(&key, &root, &root_key)
            .expect("leaf certificate");
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_no_client_auth()
                .with_single_cert(
                    vec![leaf.der().clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .expect("TLS identity");
        Self {
            root: root.der().to_vec(),
            server: Arc::new(config),
        }
    }

    fn request(&self, config: EngineConfig) -> Completion {
        let manual = config.run_mode() == nbreq::RunMode::Manual;
        let engine_result = Engine::new(config);
        let mut engine = engine_result.expect("Engine construction");
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture bind");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("fixture address");
        let server_config = Arc::clone(&self.server);
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let stream: TcpStream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("blocking accepted socket");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read bound");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write bound");
            let connection = ServerConnection::new(server_config).expect("server TLS");
            let mut stream = StreamOwned::new(connection, stream);
            let mut head = Vec::new();
            let mut bytes = [0; 1024];
            while !head.windows(4).any(|part| part == b"\r\n\r\n") {
                match stream.read(&mut bytes) {
                    Ok(0) | Err(_) => return, // An untrusted/invalid peer is expected to reject TLS.
                    Ok(read) => head.extend_from_slice(&bytes[..read]),
                }
                assert!(head.len() <= 8192);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("fixture response");
            stream.flush().expect("TLS flush");
        });
        let pending = engine
            .client()
            .submit(
                Request::get(format!("https://{address}/"))
                    .total_timeout(Duration::from_secs(6))
                    .build()
                    .expect("request"),
            )
            .expect("submit");
        let result = if manual {
            engine.drive_until(pending).expect("manual drive")
        } else {
            pending.wait()
        };
        engine.shutdown().expect("Engine joins");
        server.join().expect("fixture joins");
        result
    }
}

fn assert_ok(result: Completion) {
    match result {
        Completion::Completed(response) => {
            assert_eq!(response.status(), 200);
            assert_eq!(response.body(), b"ok");
        }
        other => panic!("private CA request must verify and succeed: {other:?}"),
    }
}

#[test]
fn additional_root_enables_private_ca_and_is_isolated_to_its_engine() {
    let identity = Identity::new("127.0.0.1", false);
    assert_ok(identity.request(
        EngineConfig::spawned().with_additional_tls_root_certificate(identity.root.clone()),
    ));
    assert!(
        matches!(
            identity.request(EngineConfig::spawned()),
            Completion::Failed(_)
        ),
        "trust must not leak to another Engine"
    );
    let unrelated = Identity::new("127.0.0.1", false);
    assert!(matches!(
        identity
            .request(EngineConfig::spawned().with_additional_tls_root_certificate(unrelated.root)),
        Completion::Failed(_)
    ));
    assert_ok(identity.request(
        EngineConfig::manual().with_additional_tls_root_certificate(identity.root.clone()),
    ));
}

#[test]
fn additional_roots_reject_malformed_certificates_at_construction() {
    for config in [EngineConfig::spawned(), EngineConfig::manual()] {
        for der in [
            Vec::new(),
            vec![1, 2, 3],
            b"-----BEGIN CERTIFICATE-----".to_vec(),
        ] {
            let result = Engine::new(config.clone().with_additional_tls_root_certificate(der));
            assert!(
                result.is_err(),
                "malformed DER must fail before starting an Engine"
            );
            let error = result.err().expect("construction error");
            assert_eq!(error.tls_failure(), Some(TlsFailure::Configuration));
        }
    }
}

#[test]
fn extra_trust_keeps_hostname_and_expiry_verification() {
    for (host, expired) in [("wrong.test", false), ("127.0.0.1", true)] {
        let identity = Identity::new(host, expired);
        let result = identity.request(
            EngineConfig::spawned().with_additional_tls_root_certificate(identity.root.clone()),
        );
        match result {
            Completion::Failed(error) => assert_eq!(
                error.tls_failure(),
                Some(if expired {
                    // The pinned Apple verifier preserves this as CertificateError::Other;
                    // it does not map errSecCertificateExpired to rustls's Expired variant.
                    if cfg!(target_vendor = "apple") {
                        TlsFailure::CertificateInvalid
                    } else {
                        TlsFailure::CertificateExpired
                    }
                } else {
                    TlsFailure::CertificateHostnameMismatch
                })
            ),
            other => panic!("invalid certificate must remain rejected: {other:?}"),
        }
    }
}

#[test]
fn multiple_roots_and_cloned_configuration_keep_both_private_authorities() {
    let first = Identity::new("127.0.0.1", false);
    let second = Identity::new("127.0.0.1", false);
    let config = EngineConfig::spawned()
        .with_additional_tls_root_certificate(first.root.clone())
        .with_additional_tls_root_certificate(second.root.clone());
    assert_ok(first.request(config.clone()));
    assert_ok(second.request(config));
}
