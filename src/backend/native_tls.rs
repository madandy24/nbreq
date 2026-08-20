#![cfg_attr(not(test), allow(dead_code))]

//! Private sans-I/O rustls client state.
//!
//! The native HTTP owner supplies encrypted bytes and owns the socket. This module owns only the
//! per-request TLS state and configuration; it never polls, spawns, waits, or calls user code.

use std::fmt;
use std::io::{self, Cursor, Read, Write};
use std::sync::Arc;

use rustls::client::ClientConnection;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::BuilderVerifierExt;

use crate::{Error, ErrorKind, TlsVerification, TransportStage};

const TLS_FLIGHT_LIMIT: usize = 512 * 1024;
const TLS_PLAINTEXT_CHUNK: usize = 16 * 1024;

#[derive(Clone)]
pub(super) struct NativeTlsConfigs {
    verified: Arc<ClientConfig>,
    unverified: Arc<ClientConfig>,
}

impl NativeTlsConfigs {
    pub(super) fn platform() -> Result<Self, Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .map_err(|error| tls_config_error("protocol versions", error))?;
        let verified = builder
            .with_platform_verifier()
            .map_err(|error| tls_config_error("platform verifier", error))?
            .with_no_client_auth();
        Self::from_verified(provider, verified)
    }

    pub(super) fn with_test_root(root: CertificateDer<'static>) -> Result<Self, Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(root)
            .map_err(|error| tls_config_error("test trust root", error))?;
        let verified = ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .map_err(|error| tls_config_error("protocol versions", error))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Self::from_verified(provider, verified)
    }

    fn from_verified(
        provider: Arc<CryptoProvider>,
        mut verified: ClientConfig,
    ) -> Result<Self, Error> {
        verified.alpn_protocols = vec![b"http/1.1".to_vec()];
        let verifier = Arc::new(NoCertificateVerification {
            algorithms: provider.signature_verification_algorithms,
        });
        let mut unverified = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| tls_config_error("protocol versions", error))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        unverified.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            verified: Arc::new(verified),
            unverified: Arc::new(unverified),
        })
    }

    pub(super) fn connection(
        &self,
        host: &str,
        policy: TlsVerification,
        request: Vec<u8>,
    ) -> Result<NativeTls, Error> {
        let name = ServerName::try_from(host.to_owned()).map_err(|_| {
            Error::transport(
                TransportStage::Tls,
                "the HTTPS hostname cannot be represented as a TLS server name",
            )
        })?;
        let config = match policy {
            TlsVerification::Verify => Arc::clone(&self.verified),
            TlsVerification::DangerouslyDisableCertificateVerification => {
                Arc::clone(&self.unverified)
            }
        };
        let connection = ClientConnection::new(config, name).map_err(|error| {
            Error::transport(
                TransportStage::Tls,
                format!("native TLS client setup failed: {error}"),
            )
        })?;
        Ok(NativeTls {
            connection,
            request: Some(request),
            handshake_received: 0,
        })
    }
}

pub(super) struct NativeTls {
    connection: ClientConnection,
    request: Option<Vec<u8>>,
    handshake_received: usize,
}

#[derive(Debug)]
pub(super) struct TlsProgress {
    pub(super) outbound: Vec<u8>,
    pub(super) plaintext: Vec<u8>,
    pub(super) handshake_complete: bool,
    pub(super) peer_closed: bool,
}

impl NativeTls {
    pub(super) fn start(&mut self) -> Result<Vec<u8>, Error> {
        self.take_outbound()
    }

    pub(super) fn receive(&mut self, encrypted: &[u8]) -> Result<TlsProgress, Error> {
        if self.connection.is_handshaking() {
            self.handshake_received = self
                .handshake_received
                .checked_add(encrypted.len())
                .ok_or_else(tls_handshake_flight_limit)?;
            if self.handshake_received > TLS_FLIGHT_LIMIT {
                return Err(tls_handshake_flight_limit());
            }
        }
        let mut input = Cursor::new(encrypted);
        let mut plaintext = Vec::new();
        let mut peer_closed = false;
        while usize::try_from(input.position()).unwrap_or(usize::MAX) < encrypted.len() {
            let was_handshaking = self.connection.is_handshaking();
            let consumed = self
                .connection
                .read_tls(&mut input)
                .map_err(|error| tls_io_error(was_handshaking, "record input", error))?;
            if consumed == 0 {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "native TLS made no progress while consuming a reactor event",
                ));
            }
            let state = self.connection.process_new_packets().map_err(|error| {
                let stage = if was_handshaking {
                    TransportStage::Tls
                } else {
                    TransportStage::Receive
                };
                Error::transport(
                    stage,
                    format!("native TLS packet processing failed: {error}"),
                )
            })?;
            peer_closed |= state.peer_has_closed();
            if !self.connection.is_handshaking() {
                if let Some(request) = self.request.take() {
                    self.connection
                        .writer()
                        .write_all(&request)
                        .map_err(|error| tls_io_error(false, "request encryption", error))?;
                }
            }
            self.drain_plaintext(&mut plaintext)?;
        }
        Ok(TlsProgress {
            outbound: self.take_outbound()?,
            plaintext,
            handshake_complete: !self.connection.is_handshaking(),
            peer_closed,
        })
    }

    fn drain_plaintext(&mut self, plaintext: &mut Vec<u8>) -> Result<(), Error> {
        let mut buffer = [0_u8; TLS_PLAINTEXT_CHUNK];
        loop {
            match self.connection.reader().read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => plaintext.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(tls_io_error(false, "plaintext read", error)),
            }
        }
        Ok(())
    }

    pub(super) fn is_handshaking(&self) -> bool {
        self.connection.is_handshaking()
    }

    fn take_outbound(&mut self) -> Result<Vec<u8>, Error> {
        let mut output = BoundedWriter::new(TLS_FLIGHT_LIMIT);
        while self.connection.wants_write() {
            let written = self.connection.write_tls(&mut output).map_err(|error| {
                tls_io_error(self.connection.is_handshaking(), "record output", error)
            })?;
            if written == 0 {
                break;
            }
        }
        Ok(output.into_inner())
    }
}

pub(super) fn encrypted_outbound_limit(request_bytes: usize) -> usize {
    request_bytes.saturating_add(TLS_FLIGHT_LIMIT)
}

pub(super) fn encrypted_receive_limit(plaintext_bytes: usize) -> usize {
    plaintext_bytes.saturating_add(TLS_FLIGHT_LIMIT)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("native TLS flight length overflow"))?;
        if next > self.limit {
            return Err(io::Error::other(
                "native TLS flight exceeds the private buffer limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct NoCertificateVerification {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn tls_config_error(operation: &str, error: impl fmt::Display) -> Error {
    Error::transport(
        TransportStage::Tls,
        format!("native TLS {operation} configuration failed: {error}"),
    )
}

fn tls_io_error(handshaking: bool, operation: &str, error: io::Error) -> Error {
    let stage = if handshaking {
        TransportStage::Tls
    } else {
        TransportStage::Receive
    };
    Error::transport(stage, format!("native TLS {operation} failed: {error}"))
}

fn tls_handshake_flight_limit() -> Error {
    Error::new(
        ErrorKind::Limit,
        "the native TLS peer handshake exceeded its private wire budget",
    )
}

#[cfg(test)]
mod tests {
    use rustls::ServerConfig;
    use rustls::ServerConnection;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

    use rcgen::{CertificateParams, KeyPair, date_time_ymd};

    use super::*;

    fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        identity_for("resolved.test", false)
    }

    fn identity_for(
        host: &str,
        expired: bool,
    ) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let key = KeyPair::generate().expect("TLS test key must generate");
        let mut params =
            CertificateParams::new(vec![host.to_owned()]).expect("TLS test parameters must build");
        if expired {
            params.not_before = date_time_ymd(2010, 1, 1);
            params.not_after = date_time_ymd(2011, 1, 1);
        }
        let cert = params
            .self_signed(&key)
            .expect("TLS test certificate must sign");
        (
            cert.der().clone(),
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
    }

    fn server_config(
        cert: CertificateDer<'static>,
        key: PrivateKeyDer<'static>,
    ) -> Arc<ServerConfig> {
        let mut config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS server versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .expect("TLS server identity must configure");
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(config)
    }

    fn first_server_flight(
        client: &mut NativeTls,
        cert: CertificateDer<'static>,
        key: PrivateKeyDer<'static>,
    ) -> Result<TlsProgress, Error> {
        let mut server =
            ServerConnection::new(server_config(cert, key)).expect("TLS server state must build");
        let client_hello = client.start().expect("ClientHello must encode");
        server
            .read_tls(&mut Cursor::new(client_hello))
            .expect("server ClientHello must read");
        server
            .process_new_packets()
            .expect("server ClientHello must process");
        let mut server_flight = Vec::new();
        while server.wants_write() {
            server
                .write_tls(&mut server_flight)
                .expect("server flight must encode");
        }
        client.receive(&server_flight)
    }

    #[test]
    fn verified_sans_io_handshake_encrypts_request_and_decrypts_response() {
        let (cert, key) = identity();
        let configs =
            NativeTlsConfigs::with_test_root(cert.clone()).expect("TLS client config must build");
        let mut client = configs
            .connection(
                "resolved.test",
                TlsVerification::Verify,
                b"GET / HTTP/1.1\r\nHost: resolved.test\r\n\r\n".to_vec(),
            )
            .expect("TLS client state must build");
        let mut server =
            ServerConnection::new(server_config(cert, key)).expect("TLS server state must build");
        let mut to_server = client.start().expect("ClientHello must encode");
        let mut received_request = Vec::new();
        let mut received_response = Vec::new();
        for _ in 0..16 {
            if !to_server.is_empty() {
                server
                    .read_tls(&mut Cursor::new(&to_server))
                    .expect("server TLS bytes must read");
                server
                    .process_new_packets()
                    .expect("server TLS packets must process");
                to_server.clear();
            }
            let mut plaintext = [0_u8; 1024];
            loop {
                match server.reader().read(&mut plaintext) {
                    Ok(0) => break,
                    Ok(read) => received_request.extend_from_slice(&plaintext[..read]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("server plaintext read failed: {error}"),
                }
            }
            if received_request.ends_with(b"\r\n\r\n") {
                server
                    .writer()
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .expect("server response must buffer");
            }
            let mut to_client = Vec::new();
            while server.wants_write() {
                server
                    .write_tls(&mut to_client)
                    .expect("server TLS bytes must encode");
            }
            if !to_client.is_empty() {
                let progress = client.receive(&to_client).expect("client TLS must advance");
                to_server = progress.outbound;
                received_response.extend_from_slice(&progress.plaintext);
            }
            if received_response.ends_with(b"ok") {
                break;
            }
        }
        assert!(received_request.starts_with(b"GET / HTTP/1.1\r\n"));
        assert!(received_response.ends_with(b"\r\n\r\nok"));
        assert!(!client.is_handshaking());
    }

    #[test]
    fn verified_wrong_host_fails_but_explicit_bypass_still_handshakes() {
        let (cert, key) = identity();
        let configs =
            NativeTlsConfigs::with_test_root(cert.clone()).expect("TLS client config must build");
        for (policy, should_succeed) in [
            (TlsVerification::Verify, false),
            (
                TlsVerification::DangerouslyDisableCertificateVerification,
                true,
            ),
        ] {
            let mut client = configs
                .connection("wrong.test", policy, Vec::new())
                .expect("TLS client state must build");
            let mut server = ServerConnection::new(server_config(cert.clone(), key.clone_key()))
                .expect("TLS server state must build");
            let client_hello = client.start().expect("ClientHello must encode");
            server
                .read_tls(&mut Cursor::new(client_hello))
                .expect("server ClientHello must read");
            server
                .process_new_packets()
                .expect("server ClientHello must process");
            let mut server_flight = Vec::new();
            while server.wants_write() {
                server
                    .write_tls(&mut server_flight)
                    .expect("server flight must encode");
            }
            assert_eq!(client.receive(&server_flight).is_ok(), should_succeed);
        }
    }

    #[test]
    fn platform_verifier_config_constructs_without_global_provider_state() {
        NativeTlsConfigs::platform().expect("platform TLS configuration must construct");
    }

    #[test]
    fn verified_unknown_root_and_expired_certificate_fail_at_tls() {
        let (trusted, _) = identity_for("resolved.test", false);
        let configs = NativeTlsConfigs::with_test_root(trusted)
            .expect("trusted TLS client config must build");

        let (unknown, unknown_key) = identity_for("resolved.test", false);
        let mut unknown_client = configs
            .connection("resolved.test", TlsVerification::Verify, Vec::new())
            .expect("unknown-root client state must build");
        let unknown_error = first_server_flight(&mut unknown_client, unknown, unknown_key)
            .expect_err("unknown root must fail");
        assert_eq!(unknown_error.transport_stage(), Some(TransportStage::Tls));

        let (expired, expired_key) = identity_for("resolved.test", true);
        let expired_configs = NativeTlsConfigs::with_test_root(expired.clone())
            .expect("expired-root client config must build");
        let mut expired_client = expired_configs
            .connection("resolved.test", TlsVerification::Verify, Vec::new())
            .expect("expired client state must build");
        let expired_error = first_server_flight(&mut expired_client, expired, expired_key)
            .expect_err("expired certificate must fail");
        assert_eq!(expired_error.transport_stage(), Some(TransportStage::Tls));
    }

    #[test]
    fn explicit_peer_alert_is_a_tls_stage_failure() {
        let (cert, _) = identity();
        let configs = NativeTlsConfigs::with_test_root(cert).expect("TLS client config must build");
        let mut client = configs
            .connection("resolved.test", TlsVerification::Verify, Vec::new())
            .expect("TLS client state must build");
        let _client_hello = client.start().expect("ClientHello must encode");

        // A plaintext TLS alert is permitted before encrypted handshake traffic begins.
        let alert = [21, 3, 3, 0, 2, 2, 40];
        let error = client
            .receive(&alert)
            .expect_err("peer handshake alert must fail");
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert_eq!(error.transport_stage(), Some(TransportStage::Tls));
    }

    #[test]
    fn incoming_and_outgoing_tls_flights_are_bounded_before_growth() {
        let mut writer = BoundedWriter::new(4);
        writer.write_all(b"four").expect("bounded write must fit");
        assert_eq!(writer.bytes.len(), 4);
        let error = writer
            .write_all(b"x")
            .expect_err("bounded writer must reject overflow");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(writer.bytes.len(), 4);

        let (cert, _) = identity();
        let configs = NativeTlsConfigs::with_test_root(cert).expect("TLS client config must build");
        let mut client = configs
            .connection("resolved.test", TlsVerification::Verify, Vec::new())
            .expect("TLS client state must build");
        let _client_hello = client.start().expect("ClientHello must encode");
        let oversized = vec![0_u8; TLS_FLIGHT_LIMIT + 1];
        let error = client
            .receive(&oversized)
            .expect_err("oversized peer handshake must fail before rustls input");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(client.handshake_received, TLS_FLIGHT_LIMIT + 1);
    }

    #[test]
    fn one_reactor_event_can_contain_more_tls_records_than_rustls_buffers_at_once() {
        let (cert, key) = identity();
        let configs =
            NativeTlsConfigs::with_test_root(cert.clone()).expect("TLS client config must build");
        let mut client = configs
            .connection("resolved.test", TlsVerification::Verify, Vec::new())
            .expect("TLS client state must build");
        let mut server =
            ServerConnection::new(server_config(cert, key)).expect("TLS server state must build");
        let mut to_server = client.start().expect("ClientHello must encode");
        for _ in 0..16 {
            if !to_server.is_empty() {
                server
                    .read_tls(&mut Cursor::new(&to_server))
                    .expect("server handshake bytes must read");
                server
                    .process_new_packets()
                    .expect("server handshake bytes must process");
            }
            let mut to_client = Vec::new();
            while server.wants_write() {
                server
                    .write_tls(&mut to_client)
                    .expect("server handshake flight must encode");
            }
            if to_client.is_empty() {
                if !client.is_handshaking() && !server.is_handshaking() {
                    break;
                }
                continue;
            }
            to_server = client
                .receive(&to_client)
                .expect("client handshake must advance")
                .outbound;
        }
        assert!(!client.is_handshaking());
        assert!(!server.is_handshaking());

        let expected = vec![b'x'; 128 * 1024];
        let mut event = Vec::new();
        for chunk in expected.chunks(TLS_PLAINTEXT_CHUNK) {
            server
                .writer()
                .write_all(chunk)
                .expect("server plaintext chunk must buffer");
            while server.wants_write() {
                server
                    .write_tls(&mut event)
                    .expect("server records must encode");
            }
        }
        let progress = client
            .receive(&event)
            .expect("one multi-record reactor event must be fully consumed");
        assert_eq!(progress.plaintext, expected);
    }
}
