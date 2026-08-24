#![cfg_attr(not(test), allow(dead_code))]

//! Private sans-I/O rustls client state.
//!
//! The native HTTP owner supplies encrypted bytes and owns the socket. This module owns only the
//! per-request TLS state and configuration; it never polls, spawns, waits, or calls user code.

use std::fmt;
use std::io::{self, Cursor, Read, Write};
use std::sync::Arc;

#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::mpsc::{Receiver, Sender};
#[cfg(test)]
use std::time::Duration;

use rustls::client::ClientConnection;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error as RustlsError, SignatureScheme,
};
use rustls_platform_verifier::BuilderVerifierExt;

use crate::{Error, ErrorKind, TlsFailure, TlsVerification, TransportStage};

pub(super) const TLS_FLIGHT_LIMIT: usize = 512 * 1024;
const TLS_PLAINTEXT_CHUNK: usize = 16 * 1024;
// TLS application records carry at most 16 KiB of plaintext. This includes generous wire
// overhead without teaching the HTTP owner how to parse TLS records itself. A streaming socket
// grants at most this much encrypted input before returning to its owner.
const TLS_STREAM_WIRE_ALLOWANCE: usize = 18 * 1024;

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

    #[cfg(test)]
    pub(super) fn with_test_root_and_verification_gate(
        root: CertificateDer<'static>,
        entered: Sender<()>,
        release: Receiver<()>,
    ) -> Result<Self, Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(root)
            .map_err(|error| tls_config_error("test trust root", error))?;
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::clone(&provider),
        )
        .build()
        .map_err(|error| tls_config_error("test verifier", error))?;
        let verifier = Arc::new(GatedFirstVerification {
            inner: verifier,
            entered,
            release: Mutex::new(release),
            first: AtomicBool::new(true),
        });
        let verified = ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .map_err(|error| tls_config_error("protocol versions", error))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
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
            Error::tls(
                TransportStage::Tls,
                classify_rustls_error(&error),
                "native TLS client setup failed",
            )
        })?;
        Ok(NativeTls {
            connection,
            request: Some(PendingPlaintext {
                bytes: request,
                offset: 0,
            }),
            retained_response: PendingPlaintext {
                bytes: Vec::new(),
                offset: 0,
            },
            handshake_received: 0,
        })
    }
}

pub(super) struct NativeTls {
    connection: ClientConnection,
    request: Option<PendingPlaintext>,
    retained_response: PendingPlaintext,
    handshake_received: usize,
}

struct PendingPlaintext {
    bytes: Vec<u8>,
    offset: usize,
}

impl PendingPlaintext {
    fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[derive(Debug)]
pub(super) struct TlsProgress {
    pub(super) outbound: Vec<u8>,
    pub(super) plaintext: Vec<u8>,
    pub(super) handshake_complete: bool,
    pub(super) peer_closed: bool,
}

#[derive(Debug)]
pub(super) struct TlsStreamProgress {
    pub(super) outbound: Vec<u8>,
    pub(super) handshake_complete: bool,
    pub(super) peer_closed: bool,
}

impl NativeTls {
    pub(super) fn start(&mut self) -> Result<Vec<u8>, Error> {
        self.take_outbound()
    }

    pub(super) fn receive(&mut self, encrypted: &[u8]) -> Result<TlsProgress, Error> {
        let started_handshaking = self.connection.is_handshaking();
        if started_handshaking {
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
                Error::tls(
                    stage,
                    classify_rustls_error(&error),
                    "native TLS packet processing failed",
                )
            })?;
            peer_closed |= state.peer_has_closed();
            self.drain_plaintext(&mut plaintext)?;
        }
        Ok(TlsProgress {
            outbound: if started_handshaking {
                self.take_outbound()?
            } else {
                Vec::new()
            },
            plaintext,
            handshake_complete: !self.connection.is_handshaking(),
            peer_closed,
        })
    }

    /// Consumes one bounded encrypted streaming window and retains any resulting application
    /// plaintext until the HTTP owner explicitly consumes it.
    ///
    /// Buffered responses may drain a large reactor event in one pass. Streaming responses must
    /// not accept a second window until reader backpressure has released all plaintext from the
    /// first.
    pub(super) fn receive_streaming(
        &mut self,
        encrypted: &[u8],
    ) -> Result<TlsStreamProgress, Error> {
        if !self.retained_response.is_empty() {
            return Err(Error::new(
                ErrorKind::Internal,
                "native TLS accepted another streaming window before retained plaintext drained",
            ));
        }
        if encrypted.len() > TLS_STREAM_WIRE_ALLOWANCE {
            return Err(Error::new(
                ErrorKind::Internal,
                "native TLS streaming input exceeded its advertised socket allowance",
            ));
        }
        let progress = self.receive(encrypted)?;
        if progress.plaintext.len() > TLS_STREAM_WIRE_ALLOWANCE {
            return Err(Error::new(
                ErrorKind::Internal,
                "native TLS produced more streaming plaintext than its bounded input window",
            ));
        }
        self.retained_response = PendingPlaintext {
            bytes: progress.plaintext,
            offset: 0,
        };
        Ok(TlsStreamProgress {
            outbound: progress.outbound,
            handshake_complete: progress.handshake_complete,
            peer_closed: progress.peer_closed,
        })
    }

    /// Returns the absolute encrypted read allowance for the next streaming socket pass.
    pub(super) fn streaming_read_allowance(&self, response_capacity: usize) -> usize {
        if !self.retained_response.is_empty()
            || (!self.connection.is_handshaking() && response_capacity == 0)
        {
            0
        } else {
            TLS_STREAM_WIRE_ALLOWANCE
        }
    }

    pub(super) fn retained_plaintext(&self) -> &[u8] {
        self.retained_response.remaining()
    }

    pub(super) fn consume_retained_plaintext(&mut self, consumed: usize) -> Result<(), Error> {
        if consumed > self.retained_response.remaining().len() {
            return Err(Error::new(
                ErrorKind::Internal,
                "native HTTP consumed beyond retained TLS plaintext",
            ));
        }
        self.retained_response.offset += consumed;
        if self.retained_response.is_empty() {
            self.retained_response.bytes.clear();
            self.retained_response.offset = 0;
        }
        Ok(())
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

    pub(super) fn begin_request(&mut self, request: Vec<u8>) -> Result<(), Error> {
        if self.connection.is_handshaking() {
            return Err(Error::new(
                ErrorKind::Internal,
                "native TLS tried to reuse a connection before its handshake completed",
            ));
        }
        if self.request.is_some() || self.connection.wants_write() {
            return Err(Error::new(
                ErrorKind::Internal,
                "native TLS tried to begin a request while prior output remained",
            ));
        }
        self.request = Some(PendingPlaintext {
            bytes: request,
            offset: 0,
        });
        Ok(())
    }

    pub(super) fn pump_request(&mut self, ciphertext_limit: usize) -> Result<Vec<u8>, Error> {
        if self.connection.is_handshaking() || ciphertext_limit == 0 {
            return Ok(Vec::new());
        }
        let mut output = Vec::new();
        self.drain_outbound_up_to(ciphertext_limit, &mut output)?;
        while output.len() < ciphertext_limit {
            if self.connection.wants_write() {
                break;
            }
            let Some(request) = self.request.as_mut() else {
                break;
            };
            if request.offset == request.bytes.len() {
                self.request = None;
                break;
            }
            let end = request
                .offset
                .saturating_add(TLS_PLAINTEXT_CHUNK)
                .min(request.bytes.len());
            let written = self
                .connection
                .writer()
                .write(&request.bytes[request.offset..end])
                .map_err(|_| {
                    Error::tls(
                        TransportStage::Send,
                        TlsFailure::Io,
                        "native TLS request encryption failed",
                    )
                })?;
            if written == 0 {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "native TLS made no progress while accepting request plaintext",
                ));
            }
            request.offset += written;
            self.drain_outbound_up_to(ciphertext_limit, &mut output)?;
        }
        if self
            .request
            .as_ref()
            .is_some_and(|request| request.offset == request.bytes.len())
            && !self.connection.wants_write()
        {
            self.request = None;
        }
        Ok(output)
    }

    pub(super) fn request_fully_encrypted(&self) -> bool {
        self.request.is_none() && !self.connection.wants_write()
    }

    fn drain_outbound_up_to(
        &mut self,
        ciphertext_limit: usize,
        output: &mut Vec<u8>,
    ) -> Result<(), Error> {
        while self.connection.wants_write() && output.len() < ciphertext_limit {
            let mut writer = CappedWriter::new(output, ciphertext_limit);
            let written = self.connection.write_tls(&mut writer).map_err(|_| {
                Error::tls(
                    TransportStage::Send,
                    TlsFailure::Io,
                    "native TLS record output failed",
                )
            })?;
            if written == 0 {
                break;
            }
        }
        Ok(())
    }

    fn take_outbound(&mut self) -> Result<Vec<u8>, Error> {
        let mut output = BoundedWriter::new(TLS_FLIGHT_LIMIT);
        while self.connection.wants_write() {
            let stage = if self.connection.is_handshaking() {
                TransportStage::Tls
            } else {
                TransportStage::Send
            };
            let written = self.connection.write_tls(&mut output).map_err(|_| {
                Error::tls(stage, TlsFailure::Io, "native TLS record output failed")
            })?;
            if written == 0 {
                break;
            }
        }
        Ok(output.into_inner())
    }
}

pub(super) const fn encrypted_outbound_limit() -> usize {
    TLS_FLIGHT_LIMIT
}

pub(super) fn encrypted_receive_limit(plaintext_bytes: usize) -> usize {
    plaintext_bytes.saturating_add(TLS_FLIGHT_LIMIT)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

struct CappedWriter<'a> {
    bytes: &'a mut Vec<u8>,
    limit: usize,
}

impl<'a> CappedWriter<'a> {
    fn new(bytes: &'a mut Vec<u8>, limit: usize) -> Self {
        Self { bytes, limit }
    }
}

impl Write for CappedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let available = self.limit.saturating_sub(self.bytes.len());
        let accepted = available.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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

#[cfg(test)]
#[derive(Debug)]
struct GatedFirstVerification {
    inner: Arc<dyn ServerCertVerifier>,
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
    first: AtomicBool,
}

#[cfg(test)]
impl ServerCertVerifier for GatedFirstVerification {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self.first.swap(false, Ordering::AcqRel) {
            let _ignored = self.entered.send(());
            let _ignored = self
                .release
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .recv_timeout(Duration::from_secs(2));
        }
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn tls_config_error(operation: &str, _error: impl fmt::Display) -> Error {
    Error::tls(
        TransportStage::Tls,
        TlsFailure::Configuration,
        format!("native TLS {operation} configuration failed"),
    )
}

fn tls_io_error(handshaking: bool, operation: &str, _error: io::Error) -> Error {
    let stage = if handshaking {
        TransportStage::Tls
    } else {
        TransportStage::Receive
    };
    Error::tls(
        stage,
        TlsFailure::Io,
        format!("native TLS {operation} failed"),
    )
}

fn classify_rustls_error(error: &RustlsError) -> TlsFailure {
    match error {
        RustlsError::InvalidCertificate(error) => classify_certificate_error(error),
        RustlsError::NoCertificatesPresented | RustlsError::UnsupportedNameType => {
            TlsFailure::CertificateInvalid
        }
        RustlsError::AlertReceived(_) => TlsFailure::PeerAlert,
        RustlsError::InappropriateMessage { .. }
        | RustlsError::InappropriateHandshakeMessage { .. }
        | RustlsError::InvalidEncryptedClientHello(_)
        | RustlsError::InvalidMessage(_)
        | RustlsError::DecryptError
        | RustlsError::EncryptError
        | RustlsError::PeerIncompatible(_)
        | RustlsError::PeerMisbehaved(_)
        | RustlsError::HandshakeNotComplete
        | RustlsError::PeerSentOversizedRecord
        | RustlsError::NoApplicationProtocol
        | RustlsError::BadMaxFragmentSize
        | RustlsError::InconsistentKeys(_) => TlsFailure::Protocol,
        _ => TlsFailure::Unknown,
    }
}

fn classify_certificate_error(error: &CertificateError) -> TlsFailure {
    match error {
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            TlsFailure::CertificateHostnameMismatch
        }
        CertificateError::UnknownIssuer => TlsFailure::CertificateUnknownIssuer,
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
            TlsFailure::CertificateExpired
        }
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
            TlsFailure::CertificateNotYetValid
        }
        CertificateError::Revoked => TlsFailure::CertificateRevoked,
        _ => TlsFailure::CertificateInvalid,
    }
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

    use rcgen::{CertificateParams, DnType, KeyPair, date_time_ymd};

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
        params.distinguished_name.push(DnType::CommonName, host);
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
            if !client.is_handshaking() {
                to_server.extend(
                    client
                        .pump_request(TLS_FLIGHT_LIMIT)
                        .expect("client request plaintext must pump"),
                );
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
    fn incremental_request_pump_caps_each_ciphertext_batch() {
        const PUMP_BUDGET: usize = 32 * 1024;
        let request = vec![b'x'; TLS_FLIGHT_LIMIT * 2 + 123];
        let (cert, key) = identity();
        let configs =
            NativeTlsConfigs::with_test_root(cert.clone()).expect("TLS client config must build");
        let mut client = configs
            .connection("resolved.test", TlsVerification::Verify, request.clone())
            .expect("TLS client state must build");
        let mut server =
            ServerConnection::new(server_config(cert, key)).expect("TLS server state must build");
        let mut to_server = client.start().expect("ClientHello must encode");
        let mut received = Vec::new();

        for _ in 0..256 {
            if !to_server.is_empty() {
                let mut input = Cursor::new(&to_server);
                while usize::try_from(input.position()).unwrap_or(usize::MAX) < to_server.len() {
                    let consumed = server
                        .read_tls(&mut input)
                        .expect("server TLS bytes must read");
                    assert_ne!(consumed, 0, "server TLS input must make progress");
                    server
                        .process_new_packets()
                        .expect("server TLS packets must process");
                    let mut plaintext = [0_u8; TLS_PLAINTEXT_CHUNK];
                    loop {
                        match server.reader().read(&mut plaintext) {
                            Ok(0) => break,
                            Ok(read) => received.extend_from_slice(&plaintext[..read]),
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) => panic!("server request plaintext failed: {error}"),
                        }
                    }
                }
                to_server.clear();
            }
            if received == request {
                break;
            }
            let mut to_client = Vec::new();
            while server.wants_write() {
                server
                    .write_tls(&mut to_client)
                    .expect("server handshake bytes must encode");
            }
            if !to_client.is_empty() {
                let progress = client.receive(&to_client).expect("client TLS must advance");
                to_server.extend(progress.outbound);
            }
            if !client.is_handshaking() {
                let pumped = client
                    .pump_request(PUMP_BUDGET)
                    .expect("request ciphertext must pump");
                assert!(
                    pumped.len() <= PUMP_BUDGET,
                    "one pump exceeded its ciphertext budget"
                );
                to_server.extend(pumped);
            }
        }

        assert_eq!(received, request);
        assert!(client.request_fully_encrypted());
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
            let result = client.receive(&server_flight);
            if should_succeed {
                assert!(result.is_ok());
            } else {
                let error = result.expect_err("verified wrong host must fail");
                assert_eq!(error.transport_stage(), Some(TransportStage::Tls));
                assert_eq!(
                    error.tls_failure(),
                    Some(TlsFailure::CertificateHostnameMismatch)
                );
                assert_eq!(error.message(), "native TLS packet processing failed");
                assert!(!error.message().contains("wrong.test"));
                assert!(!error.message().contains("resolved.test"));
            }
        }
    }

    #[test]
    fn platform_verifier_config_constructs_without_global_provider_state() {
        NativeTlsConfigs::platform().expect("platform TLS configuration must construct");
    }

    #[test]
    fn verified_unknown_root_and_expired_certificate_fail_at_tls() {
        let (trusted, _) = identity_for("trusted-root.test", false);
        let configs = NativeTlsConfigs::with_test_root(trusted)
            .expect("trusted TLS client config must build");

        let (unknown, unknown_key) = identity_for("resolved.test", false);
        let mut unknown_client = configs
            .connection("resolved.test", TlsVerification::Verify, Vec::new())
            .expect("unknown-root client state must build");
        let unknown_error = first_server_flight(&mut unknown_client, unknown, unknown_key)
            .expect_err("unknown root must fail");
        assert_eq!(unknown_error.transport_stage(), Some(TransportStage::Tls));
        assert_eq!(
            unknown_error.tls_failure(),
            Some(TlsFailure::CertificateUnknownIssuer)
        );
        assert_eq!(
            unknown_error.message(),
            "native TLS packet processing failed"
        );

        let (expired, expired_key) = identity_for("resolved.test", true);
        let expired_configs = NativeTlsConfigs::with_test_root(expired.clone())
            .expect("expired-root client config must build");
        let mut expired_client = expired_configs
            .connection("resolved.test", TlsVerification::Verify, Vec::new())
            .expect("expired client state must build");
        let expired_error = first_server_flight(&mut expired_client, expired, expired_key)
            .expect_err("expired certificate must fail");
        assert_eq!(expired_error.transport_stage(), Some(TransportStage::Tls));
        assert_eq!(
            expired_error.tls_failure(),
            Some(TlsFailure::CertificateExpired)
        );
        assert_eq!(
            expired_error.message(),
            "native TLS packet processing failed"
        );
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
        assert_eq!(error.tls_failure(), Some(TlsFailure::PeerAlert));
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

    #[test]
    fn streaming_tls_retains_one_bounded_wire_window_before_reopening_reads() {
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

        let expected = vec![b's'; 64 * 1024];
        server
            .writer()
            .write_all(&expected)
            .expect("server plaintext must buffer");
        let mut encrypted = Vec::new();
        while server.wants_write() {
            server
                .write_tls(&mut encrypted)
                .expect("server records must encode");
        }

        let mut received = Vec::new();
        for wire_window in encrypted.chunks(TLS_STREAM_WIRE_ALLOWANCE) {
            assert_eq!(
                client.streaming_read_allowance(100),
                TLS_STREAM_WIRE_ALLOWANCE,
                "an empty retained window must reopen exactly one bounded socket allowance"
            );
            let progress = client
                .receive_streaming(wire_window)
                .expect("bounded TLS window must decode");
            assert!(progress.outbound.is_empty());
            assert!(progress.handshake_complete);
            assert!(!progress.peer_closed);
            while !client.retained_plaintext().is_empty() {
                assert_eq!(
                    client.streaming_read_allowance(100),
                    0,
                    "retained plaintext must close the socket allowance"
                );
                let take = client.retained_plaintext().len().min(100);
                received.extend_from_slice(&client.retained_plaintext()[..take]);
                client
                    .consume_retained_plaintext(take)
                    .expect("retained plaintext consumption must stay in bounds");
            }
        }
        assert_eq!(received, expected);
        assert_eq!(client.streaming_read_allowance(0), 0);
        assert_eq!(
            client.streaming_read_allowance(1),
            TLS_STREAM_WIRE_ALLOWANCE
        );

        let oversized = vec![0_u8; TLS_STREAM_WIRE_ALLOWANCE + 1];
        let error = client
            .receive_streaming(&oversized)
            .expect_err("input above the advertised allowance must fail closed");
        assert_eq!(error.kind(), ErrorKind::Internal);
    }
}
