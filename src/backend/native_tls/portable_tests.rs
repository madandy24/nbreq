//! Offline policy tests against the same verifier constructed for production portable modes.

use super::*;

fn unrelated_root() -> Vec<u8> {
    let key = rcgen::KeyPair::generate().expect("unrelated CA key");
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    params
        .self_signed(&key)
        .expect("unrelated CA certificate")
        .der()
        .to_vec()
}

fn verify_public_chain(
    verifier: &dyn ServerCertVerifier,
    name: &str,
    at: u64,
) -> Result<ServerCertVerified, RustlsError> {
    let leaf = CertificateDer::from(
        include_bytes!("../../../tests/fixtures/portable-trust/cavesvr3-chain-0.der").as_slice(),
    );
    let intermediates = [
        CertificateDer::from(
            include_bytes!("../../../tests/fixtures/portable-trust/cavesvr3-chain-1.der")
                .as_slice(),
        ),
        CertificateDer::from(
            include_bytes!("../../../tests/fixtures/portable-trust/cavesvr3-chain-2.der")
                .as_slice(),
        ),
    ];
    let name = ServerName::try_from(name.to_owned()).expect("certificate identity");
    verifier.verify_server_cert(
        &leaf,
        &intermediates,
        &name,
        &[],
        UnixTime::since_unix_epoch(std::time::Duration::from_secs(at)),
    )
}

// Historical public chain at its captured valid time. No DNS, sockets, clock dependence,
// platform-root changes, TLS sessions or private keys are involved in these tests.
const CAPTURE_TIME: u64 = 1_790_385_456;
const HOST: &str = "cavesvr3.caverock.com";

#[test]
fn held_backend_rejects_explicit_trust_it_cannot_apply() {
    for base in [EngineConfig::spawned(), EngineConfig::manual()] {
        let (engine, _) = crate::testing::engine(base.clone()).expect("ordinary held backend");
        engine.shutdown().expect("held Engine shutdown");
        for config in [
            base.clone().with_tls_trust(TlsTrust::SuppliedRootsOnly),
            base.clone().with_tls_trust(TlsTrust::BundledMozilla),
            base.with_additional_tls_root_certificate(unrelated_root()),
        ] {
            let error = crate::testing::engine(config)
                .err()
                .expect("held backend must not ignore TLS trust configuration");
            assert_eq!(error.kind(), ErrorKind::Unsupported);
        }
    }
}

#[test]
fn supplied_only_verifier_does_not_import_public_anchors() {
    let config = EngineConfig::spawned()
        .with_additional_tls_root_certificate(unrelated_root())
        .with_tls_trust(TlsTrust::SuppliedRootsOnly);
    let verifier = portable_verifier(&config, Arc::new(rustls::crypto::ring::default_provider()))
        .expect("supplied-only verifier");
    assert!(matches!(
        verify_public_chain(verifier.as_ref(), HOST, CAPTURE_TIME),
        Err(RustlsError::InvalidCertificate(
            CertificateError::UnknownIssuer
        ))
    ));
}

#[cfg(feature = "bundled-roots")]
#[test]
fn bundled_verifier_accepts_real_public_chain_and_retains_identity_and_time_checks() {
    for custom in [None, Some(unrelated_root())] {
        let mut config = EngineConfig::spawned().with_tls_trust(TlsTrust::BundledMozilla);
        if let Some(root) = custom {
            config = config.with_additional_tls_root_certificate(root);
        }
        let verifier =
            portable_verifier(&config, Arc::new(rustls::crypto::ring::default_provider()))
                .expect("bundled verifier");
        verify_public_chain(verifier.as_ref(), HOST, CAPTURE_TIME)
            .expect("Mozilla anchors must verify the captured public chain");
        assert!(matches!(
            verify_public_chain(verifier.as_ref(), "wrong.test", CAPTURE_TIME),
            Err(RustlsError::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }
            ))
        ));
        assert!(matches!(
            verify_public_chain(verifier.as_ref(), HOST, CAPTURE_TIME + 366 * 86400),
            Err(RustlsError::InvalidCertificate(
                CertificateError::Expired | CertificateError::ExpiredContext { .. }
            ))
        ));
        // Switching the source on a clone must discard implicit Mozilla anchors while
        // preserving only the explicit additional root, never cache an earlier verifier.
        let supplied = config
            .clone()
            .with_additional_tls_root_certificate(unrelated_root())
            .with_tls_trust(TlsTrust::SuppliedRootsOnly);
        let verifier = portable_verifier(
            &supplied,
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .expect("switched supplied-only verifier");
        assert!(matches!(
            verify_public_chain(verifier.as_ref(), HOST, CAPTURE_TIME),
            Err(RustlsError::InvalidCertificate(
                CertificateError::UnknownIssuer
            ))
        ));
    }
}
