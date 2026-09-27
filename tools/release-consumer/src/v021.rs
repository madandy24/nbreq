#[test]
fn bundled_roots_policy_is_explicit() {
    use nbreq::{Engine, EngineConfig, TlsTrust};

    // Compiling the optional bundle must not change the Engine's default trust policy.
    let config = EngineConfig::spawned();
    assert_eq!(config.tls_trust(), TlsTrust::Platform);
    let config = config.with_tls_trust(TlsTrust::BundledMozilla);
    assert_eq!(config.tls_trust(), TlsTrust::BundledMozilla);

    #[cfg(feature = "bundled-roots")]
    Engine::new(config)
        .expect("bundled roots support explicit construction")
        .shutdown()
        .unwrap();

    #[cfg(not(feature = "bundled-roots"))]
    assert_eq!(
        Engine::new(config)
            .err()
            .expect("bundle-disabled selection must be rejected")
            .kind(),
        nbreq::ErrorKind::Unsupported,
    );
}
