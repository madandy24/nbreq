//! Native HTTP transport owner and protocol coordinator.
//!
//! This module owns DNS, sockets, TLS, pooling, deadlines, uploads, response delivery, and
//! standalone TCP. HTTP/1.1 wire mechanics live in `http1`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(feature = "resolver")]
use super::BackendResolveCompletion;
use super::native::{
    NATIVE_SAFETY_POLL, NativeEvent, NativeFailure, NativeFailureKind, NativeReactor, SlotId,
};
#[cfg(test)]
use super::native_dns::ResolverSnapshot;
use super::native_dns::{
    NativeResolver, PublicLookupOutcome, PublicResolveSpec, ResolveKey, ResolverConfig,
};
use super::native_tls::{
    NativeTls, NativeTlsConfigs, TlsProgress, TlsStreamProgress, encrypted_outbound_limit,
    encrypted_receive_limit,
};
use super::{Backend, BackendCompletion, BackendFactory, PollMode};
use crate::metrics::Metrics;
use crate::registry::{Shared, TcpConnectSink};
use crate::stream::{ResponseSink, UploadBody, UploadFraming, UploadPoll};
use crate::tcp::io::TcpIoOwner;
use crate::types::{http_origin, redirected_request};
use crate::{
    AddressFamily, AddressOrder, CacheMode, Completion, EngineConfig, Error, ErrorKind, Header,
    LimitKind, Request, RequestId, ResolveStatus, Response, ShutdownError, StreamRequest,
    TcpConnectRequest, TcpConnectTarget, TimeoutKind, TlsFailure, TransportStage,
};
#[cfg(feature = "resolver")]
use crate::{ResolveCompletion, ResolveRequest, ResolveResponse, ResolvedAddress};

const MAX_INFORMATIONAL_RESPONSES: u8 = 8;

#[derive(Clone, Copy)]
struct ConnectionLimits {
    global: usize,
    per_origin: usize,
    idle_global: usize,
    idle_per_origin: usize,
    idle_timeout: Duration,
}

impl ConnectionLimits {
    fn from_config(config: &EngineConfig) -> Self {
        Self {
            global: config.max_connections().get(),
            per_origin: config.max_connections_per_origin().get(),
            idle_global: config.max_idle_connections(),
            idle_per_origin: config.max_idle_connections_per_origin(),
            idle_timeout: config.idle_connection_timeout(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct HttpLimits {
    pub(super) body_bytes: usize,
    pub(super) header_bytes: usize,
    pub(super) header_count: usize,
}

impl HttpLimits {
    fn from_config(config: &EngineConfig) -> Self {
        Self {
            body_bytes: config.max_response_body_bytes(),
            header_bytes: config.max_header_bytes(),
            header_count: config.max_header_count(),
        }
    }

    fn reactor_receive_limit(self) -> usize {
        self.header_bytes
            .saturating_mul(usize::from(MAX_INFORMATIONAL_RESPONSES) + 2)
            .saturating_add(self.body_bytes)
            .saturating_add(16 * 1024)
    }
}

#[cfg(any(test, fuzzing))]
mod fuzz;
mod http1;

#[cfg(any(test, fuzzing))]
pub(crate) use fuzz::{fuzz_response_decoder, fuzz_streaming_response_decoder};
#[cfg(test)]
use http1::{BodyFraming, ParsedHead, parse_response_head, serialize_request};
use http1::{
    DecodeProgress, ResponseDecoder, SerializedRequest, StreamDecodeProgress, StreamHeadDecision,
    StreamingResponseDecoder, serialize_request_with_upload,
};

fn resolved_redirect_target(
    request: &Request,
    response: &Response,
) -> Result<Option<String>, Error> {
    resolved_redirect_target_from_headers(request, response.headers())
}

fn resolved_redirect_target_from_headers(
    request: &Request,
    headers: &[Header],
) -> Result<Option<String>, Error> {
    let mut location = None;
    for header in headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("location"))
    {
        if location.is_some() {
            return Err(Error::new(
                ErrorKind::Redirect,
                "a redirect response contained more than one Location field",
            ));
        }
        let value = header.value().trim_ascii_start().trim_ascii_end();
        location = Some(std::str::from_utf8(value).map_err(|_| {
            Error::new(
                ErrorKind::Redirect,
                "a redirect Location was not valid UTF-8",
            )
        })?);
    }
    let Some(location) = location else {
        return Ok(None);
    };
    let base = url::Url::parse(request.url()).map_err(|_| {
        Error::new(
            ErrorKind::Redirect,
            "the redirect source URL could not be resolved",
        )
    })?;
    let target = base.join(location).map_err(|_| {
        Error::new(
            ErrorKind::Redirect,
            "the redirect Location could not be resolved",
        )
    })?;
    Ok(Some(target.into()))
}

/// Native HTTP factory with the accepted DNS, TCP, TLS, redirect, pooling, and streaming owners.
/// Ordinary `Engine::new` selects this implementation when the default `native` feature is built.
#[allow(dead_code)]
pub(super) struct NativeHttpFactory {
    limits: HttpLimits,
    connection_limits: ConnectionLimits,
    resolver: Option<ResolverConfig>,
    tls: Option<NativeTlsConfigs>,
}

#[allow(dead_code)]
impl NativeHttpFactory {
    pub(super) fn new(config: &EngineConfig) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: None,
            tls: None,
        }
    }

    pub(super) fn new_with_nameserver(config: &EngineConfig, nameserver: SocketAddr) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: None,
        }
    }

    pub(super) fn new_with_nameserver_and_search_suffixes(
        config: &EngineConfig,
        nameserver: SocketAddr,
        suffixes: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver).with_search_suffixes(suffixes)),
            tls: None,
        }
    }

    #[cfg(test)]
    pub(super) fn new_with_refreshable_nameserver(
        config: &EngineConfig,
        nameserver: SocketAddr,
        refresh_interval: Duration,
    ) -> (
        Self,
        std::sync::mpsc::Sender<Result<ResolverSnapshot, Error>>,
    ) {
        let (resolver, updates) = ResolverConfig::injected(nameserver)
            .with_test_refresh_source_and_interval(Some(refresh_interval));
        (
            Self {
                limits: HttpLimits::from_config(config),
                connection_limits: ConnectionLimits::from_config(config),
                resolver: Some(resolver),
                tls: None,
            },
            updates,
        )
    }

    pub(super) fn new_with_system_dns(config: &EngineConfig) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::system()?),
            tls: None,
        })
    }

    pub(super) fn new_with_nameserver_and_platform_tls(
        config: &EngineConfig,
        nameserver: SocketAddr,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: Some(NativeTlsConfigs::platform()?),
        })
    }

    pub(super) fn new_with_system_dns_and_platform_tls(
        config: &EngineConfig,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::system()?),
            tls: Some(NativeTlsConfigs::platform()?),
        })
    }

    pub(super) fn new_with_nameserver_and_test_root(
        config: &EngineConfig,
        nameserver: SocketAddr,
        root_der: Vec<u8>,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: Some(NativeTlsConfigs::with_test_root(root_der.into())?),
        })
    }

    #[cfg(test)]
    pub(super) fn new_with_nameserver_and_verification_gate(
        config: &EngineConfig,
        nameserver: SocketAddr,
        root_der: Vec<u8>,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: Some(NativeTlsConfigs::with_test_root_and_verification_gate(
                root_der.into(),
                entered,
                release,
            )?),
        })
    }

    pub(super) fn into_backend(self) -> Result<Box<dyn Backend + Send>, Error> {
        Ok(Box::new(NativeHttpBackend::new(
            self.limits,
            self.resolver,
            self.tls,
            self.connection_limits,
        )?))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_write_limit(
        self,
        bytes: usize,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.reactor.limit_writes_for_test(bytes);
        Ok(Box::new(backend))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_failed_standalone_addresses(
        self,
        count: usize,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.failed_standalone_addresses_remaining = count;
        Ok(Box::new(backend))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_delayed_failed_standalone_address(
        self,
        delay: Duration,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.failed_standalone_addresses_remaining = 1;
        backend.failed_standalone_address_delay = Some(delay);
        Ok(Box::new(backend))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_standalone_dns_handoff_gate(
        self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.standalone_dns_handoff_gate = Some((entered, release));
        Ok(Box::new(backend))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_standalone_socket_gate(
        self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.standalone_socket_gate = Some((entered, release));
        Ok(Box::new(backend))
    }
}

impl BackendFactory for NativeHttpFactory {
    fn connection_metrics_available(&self) -> bool {
        true
    }

    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.attach_metrics(Arc::clone(&shared.metrics));
        let waker = backend.reactor.waker();
        shared.queue.set_external_waker(Some(Arc::new(move || {
            waker.wake().map_err(|error| {
                Error::new(
                    ErrorKind::Internal,
                    format!("native HTTP command wake failed: {error}"),
                )
            })
        })));
        Ok(Box::new(backend))
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_native_resolver(&self) -> bool {
        self.resolver.is_some()
    }

    fn supports_standalone_tcp(&self) -> bool {
        true
    }
}

struct HttpTransfer {
    request_id: RequestId,
    response: TransferResponse,
    body_bearing: bool,
    response_started: bool,
    connected: bool,
    tls: Option<NativeTls>,
    connect_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    inactivity_timeout: Option<Duration>,
    inactivity_deadline: Option<Instant>,
    inactivity_paused: bool,
    key: ConnectionKey,
    resolver_config_generation: u64,
    request_permits_reuse: bool,
    request_write_drained: bool,
    request: Request,
    redirect_hops: u8,
    upload: Option<NativeUpload>,
    upload_aborted: bool,
}

enum TransferResponse {
    Buffered(ResponseDecoder),
    Streaming {
        decoder: StreamingResponseDecoder,
        retained_cleartext: Vec<u8>,
        retained_offset: usize,
        peer_closed: bool,
        tls_dirty_eof: bool,
        redirect: Option<Request>,
    },
}

impl TransferResponse {
    fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming { .. })
    }
}

struct NativeUpload {
    body: UploadBody,
    framing: UploadFraming,
    producer_finished: bool,
    pending_wire: Option<Vec<u8>>,
}

enum UploadWire {
    Pending,
    Bytes(Vec<u8>),
    Complete,
}

impl NativeUpload {
    fn new(body: UploadBody) -> Self {
        let framing = body.framing();
        Self {
            body,
            framing,
            producer_finished: false,
            pending_wire: None,
        }
    }

    fn next_wire(&mut self, capacity: Option<usize>) -> Result<UploadWire, Error> {
        if self.pending_wire.is_none() && !self.producer_finished {
            self.prepare_wire()?;
        }
        if let Some(wire) = self.pending_wire.take() {
            if capacity.is_some_and(|capacity| wire.len() > capacity) {
                self.pending_wire = Some(wire);
                return Ok(UploadWire::Pending);
            }
            return Ok(UploadWire::Bytes(wire));
        }
        if self.producer_finished {
            Ok(UploadWire::Complete)
        } else {
            Ok(UploadWire::Pending)
        }
    }

    fn prepare_wire(&mut self) -> Result<(), Error> {
        match self.body.try_pop() {
            UploadPoll::Chunk(chunk) => match self.framing {
                UploadFraming::Fixed(_) => self.pending_wire = Some(chunk),
                UploadFraming::Chunked => {
                    let prefix = format!("{:X}\r\n", chunk.len());
                    let capacity = prefix
                        .len()
                        .checked_add(chunk.len())
                        .and_then(|length| length.checked_add(2))
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::Internal,
                                "native chunked upload framing length overflowed",
                            )
                        })?;
                    let mut wire = Vec::with_capacity(capacity);
                    wire.extend_from_slice(prefix.as_bytes());
                    wire.extend_from_slice(&chunk);
                    wire.extend_from_slice(b"\r\n");
                    self.pending_wire = Some(wire);
                }
            },
            UploadPoll::Pending => {}
            UploadPoll::Finished => match self.framing {
                UploadFraming::Fixed(_) => {
                    self.producer_finished = true;
                }
                UploadFraming::Chunked => {
                    self.producer_finished = true;
                    self.pending_wire = Some(b"0\r\n\r\n".to_vec());
                }
            },
            UploadPoll::Failed(error) => return Err(error),
        }
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.producer_finished && self.pending_wire.is_none()
    }

    fn close(&mut self) {
        self.body.close();
    }
}

fn cleartext_outbound_limit(serialized: &SerializedRequest, upload: Option<&UploadBody>) -> usize {
    upload.map_or(serialized.bytes.len(), |upload| {
        serialized
            .bytes
            .len()
            .saturating_add(upload.queue_capacity())
            .saturating_add(32)
    })
}

impl HttpTransfer {
    fn next_deadline(&self) -> Option<Instant> {
        [
            self.total_deadline,
            (!self.connected).then_some(self.connect_deadline).flatten(),
            (!self.inactivity_paused)
                .then_some(self.inactivity_deadline)
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn note_progress(&mut self, now: Instant, connected: bool) -> Option<Instant> {
        self.connected |= connected;
        self.inactivity_paused = false;
        self.inactivity_deadline = self
            .inactivity_timeout
            .and_then(|timeout| now.checked_add(timeout));
        self.next_deadline()
    }

    fn expired_timeout(&self, now: Instant) -> TimeoutKind {
        if self.total_deadline.is_some_and(|deadline| deadline <= now) {
            TimeoutKind::Total
        } else if !self.connected
            && self
                .connect_deadline
                .is_some_and(|deadline| deadline <= now)
        {
            TimeoutKind::Connect
        } else if self
            .inactivity_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            TimeoutKind::Inactivity
        } else {
            TimeoutKind::Unknown
        }
    }
}

struct NativeHttpBackend {
    reactor: NativeReactor,
    resolver: Option<NativeResolver>,
    tls: Option<NativeTlsConfigs>,
    limits: HttpLimits,
    request_to_slot: HashMap<RequestId, SlotId>,
    transfers: HashMap<SlotId, HttpTransfer>,
    request_to_resolve: HashMap<RequestId, ResolveKey>,
    resolves: HashMap<ResolveKey, PendingResolve>,
    #[cfg(feature = "resolver")]
    request_to_public: HashMap<RequestId, ResolveKey>,
    #[cfg(feature = "resolver")]
    public_lookups: HashMap<ResolveKey, PublicLookup>,
    #[cfg(feature = "resolver")]
    pending_public: Vec<BackendResolveCompletion>,
    standalone_request_to_resolve: HashMap<RequestId, ResolveKey>,
    standalone_resolves: HashMap<ResolveKey, StandaloneResolve>,
    pending_http_from_dns: Vec<BackendCompletion>,
    next_resolve_key: u64,
    idle: HashMap<ConnectionKey, VecDeque<IdleConnection>>,
    idle_slots: HashMap<SlotId, ConnectionKey>,
    idle_count: usize,
    resolver_config_generation: u64,
    connection_count: usize,
    connections_per_key: HashMap<ConnectionKey, usize>,
    waiting: VecDeque<PendingResolve>,
    connection_limits: ConnectionLimits,
    metrics: Option<Arc<Metrics>>,
    standalone_request_to_slot: HashMap<RequestId, SlotId>,
    standalone_pending: HashMap<SlotId, StandalonePending>,
    standalone_live: HashMap<SlotId, StandaloneTcp>,
    #[cfg(test)]
    failed_standalone_addresses_remaining: usize,
    #[cfg(test)]
    failed_standalone_address_delay: Option<Duration>,
    #[cfg(test)]
    standalone_dns_handoff_gate:
        Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    #[cfg(test)]
    standalone_socket_gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    #[cfg(test)]
    wait_for_idle_probe_data: bool,
}

struct StandaloneResolve {
    sink: TcpConnectSink,
    port: u16,
    connect_deadline: Option<Instant>,
}

struct StandalonePending {
    sink: TcpConnectSink,
    remaining: VecDeque<SocketAddr>,
    connect_deadline: Option<Instant>,
}

impl StandalonePending {
    fn id(&self) -> RequestId {
        self.sink.id()
    }

    fn is_terminal(&self) -> bool {
        self.sink.is_terminal()
    }

    fn connected(&self, local: SocketAddr, peer: SocketAddr) -> Option<TcpIoOwner> {
        self.sink.connected(local, peer)
    }

    fn read_inactivity_timeout(&self) -> Option<Duration> {
        self.sink.read_inactivity_timeout()
    }

    fn write_inactivity_timeout(&self) -> Option<Duration> {
        self.sink.write_inactivity_timeout()
    }
}

struct StandaloneTcp {
    request_id: RequestId,
    owner: TcpIoOwner,
    read_inactivity_timeout: Option<Duration>,
    read_inactivity_deadline: Option<Instant>,
    read_inactivity_paused: bool,
    write_inactivity_timeout: Option<Duration>,
    write_inactivity_deadline: Option<Instant>,
    write_inactivity_active: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StandaloneInactivity {
    Read,
    Write,
}

impl StandaloneTcp {
    fn new(
        request_id: RequestId,
        owner: TcpIoOwner,
        read_inactivity_timeout: Option<Duration>,
        write_inactivity_timeout: Option<Duration>,
        now: Instant,
    ) -> Self {
        Self {
            request_id,
            owner,
            read_inactivity_timeout,
            read_inactivity_deadline: read_inactivity_timeout
                .and_then(|timeout| now.checked_add(timeout)),
            read_inactivity_paused: false,
            write_inactivity_timeout,
            write_inactivity_deadline: None,
            write_inactivity_active: false,
        }
    }

    fn sync_pressure(&mut self, now: Instant) {
        if self.owner.read_allowance() == 0 {
            self.read_inactivity_paused = true;
            self.read_inactivity_deadline = None;
        } else if self.read_inactivity_paused {
            self.read_inactivity_paused = false;
            self.read_inactivity_deadline = self
                .read_inactivity_timeout
                .and_then(|timeout| now.checked_add(timeout));
        }

        let output_waiting = self.owner.send_occupancy() != 0;
        match (self.write_inactivity_active, output_waiting) {
            (false, true) => {
                self.write_inactivity_active = true;
                self.write_inactivity_deadline = self
                    .write_inactivity_timeout
                    .and_then(|timeout| now.checked_add(timeout));
            }
            (true, false) => {
                self.write_inactivity_active = false;
                self.write_inactivity_deadline = None;
            }
            _ => {}
        }
    }

    fn note_read_progress(&mut self, now: Instant) {
        if !self.read_inactivity_paused {
            self.read_inactivity_deadline = self
                .read_inactivity_timeout
                .and_then(|timeout| now.checked_add(timeout));
        }
    }

    fn note_write_progress(&mut self, now: Instant) {
        let output_waiting = self.owner.send_occupancy() != 0;
        self.write_inactivity_active = output_waiting;
        self.write_inactivity_deadline = output_waiting
            .then_some(self.write_inactivity_timeout)
            .flatten()
            .and_then(|timeout| now.checked_add(timeout));
    }

    fn note_peer_closed(&mut self) {
        self.read_inactivity_paused = true;
        self.read_inactivity_deadline = None;
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.read_inactivity_deadline,
            self.write_inactivity_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn expired(&self, now: Instant) -> Option<StandaloneInactivity> {
        if self
            .read_inactivity_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            Some(StandaloneInactivity::Read)
        } else if self
            .write_inactivity_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            Some(StandaloneInactivity::Write)
        } else {
            None
        }
    }
}

#[cfg(feature = "resolver")]
struct PublicLookup {
    request_id: RequestId,
    total_deadline: Option<Instant>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ConnectionKey {
    scheme: String,
    host: String,
    port: u16,
    dangerously_disable_tls_verification: bool,
}

struct IdleConnection {
    slot: SlotId,
    tls: Option<NativeTls>,
    expires_at: Instant,
}

struct PendingResolve {
    request_id: RequestId,
    serialized: SerializedRequest,
    port: u16,
    scheme: String,
    host: String,
    tls_verification: crate::TlsVerification,
    body_bearing: bool,
    connect_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    inactivity_timeout: Option<Duration>,
    inactivity_deadline: Option<Instant>,
    key: ConnectionKey,
    request: Request,
    redirect_hops: u8,
    response: PendingResponse,
}

enum PendingResponse {
    Buffered,
    Streaming {
        response: ResponseSink,
        upload: Option<UploadBody>,
    },
}

impl PendingResponse {
    fn upload(&self) -> Option<&UploadBody> {
        match self {
            Self::Buffered => None,
            Self::Streaming { upload, .. } => upload.as_ref(),
        }
    }

    fn fail(self, error: Error) -> Option<Completion> {
        match self {
            Self::Buffered => Some(Completion::Failed(error)),
            Self::Streaming {
                mut response,
                upload: _,
            } => {
                response.fail(error);
                None
            }
        }
    }

    fn into_active(
        self,
        response_to_head: bool,
        limits: HttpLimits,
    ) -> (TransferResponse, Option<NativeUpload>) {
        match self {
            Self::Buffered => (
                TransferResponse::Buffered(ResponseDecoder::new(response_to_head, limits)),
                None,
            ),
            Self::Streaming { response, upload } => (
                TransferResponse::Streaming {
                    decoder: StreamingResponseDecoder::new(response_to_head, limits, response),
                    retained_cleartext: Vec::new(),
                    retained_offset: 0,
                    peer_closed: false,
                    tls_dirty_eof: false,
                    redirect: None,
                },
                upload.map(NativeUpload::new),
            ),
        }
    }
}

#[derive(Clone, Copy)]
struct PendingDeadlines {
    connect: Option<Instant>,
    total: Option<Instant>,
    inactivity: Option<Instant>,
}

impl PendingResolve {
    fn fail(self, error: Error) -> Option<Completion> {
        self.response.fail(error)
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.connect_deadline,
            self.total_deadline,
            self.inactivity_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn expired_timeout(&self, now: Instant) -> TimeoutKind {
        if self.total_deadline.is_some_and(|deadline| deadline <= now) {
            TimeoutKind::Total
        } else if self
            .connect_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            TimeoutKind::Connect
        } else if self
            .inactivity_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            TimeoutKind::Inactivity
        } else {
            TimeoutKind::Unknown
        }
    }
}

impl NativeHttpBackend {
    fn new(
        limits: HttpLimits,
        resolver: Option<ResolverConfig>,
        tls: Option<NativeTlsConfigs>,
        connection_limits: ConnectionLimits,
    ) -> Result<Self, Error> {
        Self::new_with_connection_limits(limits, resolver, tls, connection_limits)
    }

    fn new_with_connection_limits(
        limits: HttpLimits,
        resolver: Option<ResolverConfig>,
        tls: Option<NativeTlsConfigs>,
        connection_limits: ConnectionLimits,
    ) -> Result<Self, Error> {
        if connection_limits.global == 0 || connection_limits.per_origin == 0 {
            return Err(Error::new(
                ErrorKind::Internal,
                "native connection limits are invalid",
            ));
        }
        let reactor = NativeReactor::new(256).map_err(native_internal_error)?;
        let resolver = resolver
            .map(|config| NativeResolver::new(config, reactor.waker()))
            .transpose()?;
        let resolver_config_generation = u64::from(resolver.is_some());
        Ok(Self {
            reactor,
            resolver,
            tls,
            limits,
            request_to_slot: HashMap::new(),
            transfers: HashMap::new(),
            request_to_resolve: HashMap::new(),
            resolves: HashMap::new(),
            #[cfg(feature = "resolver")]
            request_to_public: HashMap::new(),
            #[cfg(feature = "resolver")]
            public_lookups: HashMap::new(),
            #[cfg(feature = "resolver")]
            pending_public: Vec::new(),
            standalone_request_to_resolve: HashMap::new(),
            standalone_resolves: HashMap::new(),
            pending_http_from_dns: Vec::new(),
            next_resolve_key: 1,
            idle: HashMap::new(),
            idle_slots: HashMap::new(),
            idle_count: 0,
            resolver_config_generation,
            connection_count: 0,
            connections_per_key: HashMap::new(),
            waiting: VecDeque::new(),
            connection_limits,
            metrics: None,
            standalone_request_to_slot: HashMap::new(),
            standalone_pending: HashMap::new(),
            standalone_live: HashMap::new(),
            #[cfg(test)]
            failed_standalone_addresses_remaining: 0,
            #[cfg(test)]
            failed_standalone_address_delay: None,
            #[cfg(test)]
            standalone_dns_handoff_gate: None,
            #[cfg(test)]
            standalone_socket_gate: None,
            #[cfg(test)]
            wait_for_idle_probe_data: false,
        })
    }

    fn can_reserve_connection(&self, key: &ConnectionKey) -> bool {
        self.connection_count < self.connection_limits.global
            && self.connections_per_key.get(key).copied().unwrap_or(0)
                < self.connection_limits.per_origin
    }

    fn reserve_connection(&mut self, key: &ConnectionKey) -> bool {
        if !self.can_reserve_connection(key) {
            return false;
        }
        self.connection_count += 1;
        *self.connections_per_key.entry(key.clone()).or_default() += 1;
        if let Some(metrics) = &self.metrics {
            metrics.connection_opened(self.connection_count);
        }
        true
    }

    fn release_connection(&mut self, key: &ConnectionKey) {
        let Some(count) = self.connections_per_key.get_mut(key) else {
            debug_assert!(false, "released an unreserved native connection");
            return;
        };
        if self.connection_count == 0 || *count == 0 {
            debug_assert!(false, "native connection reservation count underflowed");
            return;
        }
        self.connection_count -= 1;
        *count -= 1;
        if *count == 0 {
            self.connections_per_key.remove(key);
        }
        if let Some(metrics) = &self.metrics {
            metrics.connection_closed(self.connection_count);
        }
    }

    fn evict_idle_for_key(&mut self, key: &ConnectionKey) -> bool {
        let slot = self
            .idle
            .get(key)
            .and_then(|connections| connections.front())
            .map(|connection| connection.slot);
        if let Some(slot) = slot {
            self.discard_idle_slot(slot);
            true
        } else {
            false
        }
    }

    fn evict_any_idle(&mut self) -> bool {
        let slot = self
            .idle
            .values()
            .find_map(|connections| connections.front())
            .map(|connection| connection.slot);
        if let Some(slot) = slot {
            self.discard_idle_slot(slot);
            true
        } else {
            false
        }
    }

    fn evict_all_idle(&mut self) {
        let slots = self.idle_slots.keys().copied().collect::<Vec<_>>();
        for slot in slots {
            self.discard_idle_slot(slot);
        }
    }

    fn apply_idle_http_eviction(&mut self) -> Result<(), Error> {
        let config_generation = match &self.resolver {
            Some(resolver) => resolver.take_idle_http_eviction()?,
            None => None,
        };
        if let Some(config_generation) = config_generation {
            self.resolver_config_generation = config_generation;
            self.evict_all_idle();
        }
        Ok(())
    }

    fn make_connection_capacity(&mut self, key: &ConnectionKey) -> bool {
        while self.connections_per_key.get(key).copied().unwrap_or(0)
            >= self.connection_limits.per_origin
        {
            if !self.evict_idle_for_key(key) {
                return false;
            }
        }
        while self.connection_count >= self.connection_limits.global {
            if !self.evict_any_idle() {
                return false;
            }
        }
        true
    }

    fn expire_waiting(&mut self) -> Vec<BackendCompletion> {
        let now = Instant::now();
        let mut completions = Vec::new();
        let mut index = 0;
        while index < self.waiting.len() {
            if self.waiting[index]
                .next_deadline()
                .is_none_or(|deadline| deadline > now)
            {
                index += 1;
                continue;
            }
            let Some(pending) = self.waiting.remove(index) else {
                break;
            };
            let timeout = pending.expired_timeout(now);
            let id = pending.request_id;
            if let Some(completion) = pending
                .response
                .fail(Error::timeout(timeout, native_timeout_message(timeout)))
            {
                completions.push(BackendCompletion { id, completion });
            }
        }
        if let Some(metrics) = &self.metrics {
            metrics.set_connection_waiters(self.waiting.len());
        }
        completions
    }

    fn dispatch_waiting(&mut self) -> Vec<BackendCompletion> {
        let mut completions = self.expire_waiting();
        let mut index = 0;
        while index < self.waiting.len() {
            let Some(pending) = self.waiting.remove(index) else {
                break;
            };
            let Some(pending) = self.try_begin_reused(pending) else {
                index = 0;
                continue;
            };
            if !self.make_connection_capacity(&pending.key)
                || !self.reserve_connection(&pending.key)
            {
                self.waiting.insert(index, pending);
                index += 1;
                continue;
            }
            let id = pending.request_id;
            if let Some(completion) = self.start_reserved(pending) {
                completions.push(BackendCompletion { id, completion });
            }
            index = 0;
        }
        if let Some(metrics) = &self.metrics {
            metrics.set_connection_waiters(self.waiting.len());
        }
        completions
    }

    fn make_pending(
        &self,
        request_id: RequestId,
        request: Request,
        deadlines: PendingDeadlines,
        redirect_hops: u8,
        origin_error_kind: ErrorKind,
        response: PendingResponse,
    ) -> Result<PendingResolve, (Error, PendingResponse)> {
        let upload_framing = response.upload().map(UploadBody::framing);
        let serialized = match serialize_request_with_upload(&request, self.limits, upload_framing)
        {
            Ok(serialized) => serialized,
            Err(error) => return Err((error, response)),
        };
        let origin = match http_origin(request.url(), origin_error_kind) {
            Ok(origin) => origin,
            Err(error) => return Err((error, response)),
        };
        let tls_verification = request.options().tls_verification;
        let inactivity_timeout = request.options().inactivity_timeout;
        let body_bearing = response.upload().is_some() || !request.body().is_empty();
        let key = ConnectionKey {
            scheme: origin.scheme.clone(),
            host: origin.host.to_ascii_lowercase(),
            port: origin.port,
            dangerously_disable_tls_verification: matches!(
                tls_verification,
                crate::TlsVerification::DangerouslyDisableCertificateVerification
            ),
        };
        Ok(PendingResolve {
            request_id,
            serialized,
            port: origin.port,
            scheme: origin.scheme,
            host: origin.host,
            tls_verification,
            body_bearing,
            connect_deadline: deadlines.connect,
            total_deadline: deadlines.total,
            inactivity_timeout,
            inactivity_deadline: deadlines.inactivity,
            key,
            request,
            redirect_hops,
            response,
        })
    }

    fn start_pending(&mut self, pending: PendingResolve) -> Option<Completion> {
        if let Err(error) = self.apply_idle_http_eviction() {
            return pending.fail(error);
        }
        let now = Instant::now();
        if pending
            .next_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            let timeout = pending.expired_timeout(now);
            return pending.fail(Error::timeout(timeout, native_timeout_message(timeout)));
        }
        let pending = self.try_begin_reused(pending)?;
        if !self.make_connection_capacity(&pending.key) || !self.reserve_connection(&pending.key) {
            self.waiting.push_back(pending);
            if let Some(metrics) = &self.metrics {
                metrics.set_connection_waiters(self.waiting.len());
            }
            return None;
        }
        self.start_reserved(pending)
    }

    fn finish(
        &mut self,
        slot: SlotId,
        completion: Completion,
        completions: &mut Vec<BackendCompletion>,
    ) {
        self.reactor.cancel(slot);
        if let Some(mut transfer) = self.transfers.remove(&slot) {
            self.request_to_slot.remove(&transfer.request_id);
            self.release_connection(&transfer.key);
            match &mut transfer.response {
                TransferResponse::Buffered(_) => completions.push(BackendCompletion {
                    id: transfer.request_id,
                    completion,
                }),
                TransferResponse::Streaming { decoder, .. } => match completion {
                    Completion::Failed(error) => decoder.fail(error),
                    Completion::Cancelled => decoder.fail(Error::new(
                        ErrorKind::Internal,
                        "native streaming transport cancelled without registry arbitration",
                    )),
                    Completion::Completed(_) => decoder.fail(Error::new(
                        ErrorKind::Internal,
                        "native streaming transport produced a buffered completion",
                    )),
                },
            }
        }
    }

    fn take_idle(&mut self, key: &ConnectionKey) -> Option<IdleConnection> {
        let connection = self.idle.get_mut(key)?.pop_front()?;
        if self.idle.get(key).is_some_and(VecDeque::is_empty) {
            self.idle.remove(key);
        }
        self.idle_slots.remove(&connection.slot);
        self.idle_count = self.idle_count.saturating_sub(1);
        if let Some(metrics) = &self.metrics {
            metrics.set_idle_connections(self.idle_count);
        }
        Some(connection)
    }

    fn discard_idle_slot(&mut self, slot: SlotId) {
        let Some(key) = self.idle_slots.remove(&slot) else {
            self.reactor.cancel(slot);
            return;
        };
        if let Some(connections) = self.idle.get_mut(&key) {
            if let Some(index) = connections
                .iter()
                .position(|connection| connection.slot == slot)
            {
                connections.remove(index);
                self.idle_count = self.idle_count.saturating_sub(1);
            }
            if connections.is_empty() {
                self.idle.remove(&key);
            }
        }
        if let Some(metrics) = &self.metrics {
            metrics.set_idle_connections(self.idle_count);
            metrics.idle_evicted();
        }
        self.reactor.cancel(slot);
        self.release_connection(&key);
    }

    fn expire_idle(&mut self, now: Instant) {
        let expired = self
            .idle
            .values()
            .flat_map(|connections| connections.iter())
            .filter(|connection| connection.expires_at <= now)
            .map(|connection| connection.slot)
            .collect::<Vec<_>>();
        for slot in expired {
            self.discard_idle_slot(slot);
        }
    }

    fn try_begin_reused(&mut self, pending: PendingResolve) -> Option<PendingResolve> {
        let Some(mut idle) = self.take_idle(&pending.key) else {
            return Some(pending);
        };
        #[cfg(test)]
        if self.wait_for_idle_probe_data {
            self.wait_for_idle_probe_data = false;
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.reactor.idle_is_quiet(idle.slot) == Ok(true) {
                assert!(
                    Instant::now() < deadline,
                    "test fixture bytes never became observable on the idle socket"
                );
                std::thread::yield_now();
            }
        }
        if self.reactor.idle_is_quiet(idle.slot) != Ok(true) {
            self.reactor.cancel(idle.slot);
            self.release_connection(&pending.key);
            return Some(pending);
        }
        let deadline = pending.next_deadline();
        let outbound_limit = if idle.tls.is_some() {
            encrypted_outbound_limit()
        } else {
            cleartext_outbound_limit(&pending.serialized, pending.response.upload())
        };
        let receive_limit = if idle.tls.is_some() {
            encrypted_receive_limit(self.limits.reactor_receive_limit())
        } else {
            self.limits.reactor_receive_limit()
        };
        if self
            .reactor
            .prepare_reuse(idle.slot, deadline, outbound_limit, receive_limit)
            .is_err()
        {
            self.reactor.cancel(idle.slot);
            self.release_connection(&pending.key);
            return Some(pending);
        }
        let outbound = match idle.tls.as_mut() {
            Some(tls) => {
                if tls.begin_request(pending.serialized.bytes.clone()).is_err() {
                    self.reactor.cancel(idle.slot);
                    self.release_connection(&pending.key);
                    return Some(pending);
                }
                match tls.pump_request(outbound_limit) {
                    Ok(outbound) => outbound,
                    Err(_) => {
                        self.reactor.cancel(idle.slot);
                        self.release_connection(&pending.key);
                        return Some(pending);
                    }
                }
            }
            None => pending.serialized.bytes.clone(),
        };
        if self.reactor.queue_write(idle.slot, &outbound).is_err() {
            self.reactor.cancel(idle.slot);
            self.release_connection(&pending.key);
            return Some(pending);
        }
        let request_id = pending.request_id;
        let (response, upload) = pending
            .response
            .into_active(pending.serialized.response_to_head, self.limits);
        self.request_to_slot.insert(request_id, idle.slot);
        self.transfers.insert(
            idle.slot,
            HttpTransfer {
                request_id,
                response,
                body_bearing: pending.body_bearing,
                response_started: false,
                connected: true,
                tls: idle.tls,
                connect_deadline: pending.connect_deadline,
                total_deadline: pending.total_deadline,
                inactivity_timeout: pending.inactivity_timeout,
                inactivity_deadline: pending.inactivity_deadline,
                inactivity_paused: false,
                key: pending.key,
                resolver_config_generation: self.resolver_config_generation,
                request_permits_reuse: pending.serialized.permits_reuse,
                request_write_drained: false,
                request: pending.request,
                redirect_hops: pending.redirect_hops,
                upload,
                upload_aborted: false,
            },
        );
        if let Some(metrics) = &self.metrics {
            metrics.connection_reused();
        }
        None
    }

    fn complete_response(
        &mut self,
        slot: SlotId,
        response: Response,
        response_permits_reuse: bool,
        peer_closed: bool,
        completions: &mut Vec<BackendCompletion>,
    ) {
        let Some(transfer) = self.transfers.remove(&slot) else {
            self.reactor.cancel(slot);
            return;
        };
        self.request_to_slot.remove(&transfer.request_id);
        let redirect = redirected_request(
            &transfer.request,
            response.status(),
            transfer.redirect_hops,
            || resolved_redirect_target(&transfer.request, &response),
        );
        let request_id = transfer.request_id;
        let total_deadline = transfer.total_deadline;
        let next_redirect_hops = transfer.redirect_hops.saturating_add(1);
        let per_origin_idle = self.idle.get(&transfer.key).map_or(0, VecDeque::len);
        let reusable = response_permits_reuse
            && transfer.request_permits_reuse
            && transfer.request_write_drained
            && transfer.resolver_config_generation == self.resolver_config_generation
            && !peer_closed
            && self.connection_limits.idle_timeout != Duration::ZERO
            && self.idle_count < self.connection_limits.idle_global
            && per_origin_idle < self.connection_limits.idle_per_origin;
        if reusable {
            let parked = Instant::now()
                .checked_add(self.connection_limits.idle_timeout)
                .filter(|idle_deadline| {
                    self.reactor
                        .set_deadline(slot, Some(*idle_deadline))
                        .is_ok()
                });
            if let Some(expires_at) = parked {
                self.idle_slots.insert(slot, transfer.key.clone());
                self.idle
                    .entry(transfer.key)
                    .or_default()
                    .push_back(IdleConnection {
                        slot,
                        tls: transfer.tls,
                        expires_at,
                    });
                self.idle_count += 1;
                if let Some(metrics) = &self.metrics {
                    metrics.set_idle_connections(self.idle_count);
                }
            } else {
                self.reactor.cancel(slot);
                self.release_connection(&transfer.key);
            }
        } else {
            self.reactor.cancel(slot);
            self.release_connection(&transfer.key);
        }
        match redirect {
            Ok(None) => completions.push(BackendCompletion {
                id: request_id,
                completion: Completion::Completed(response),
            }),
            Err(error) => completions.push(BackendCompletion {
                id: request_id,
                completion: Completion::Failed(error),
            }),
            Ok(Some(request)) => {
                let now = Instant::now();
                let connect_deadline = request
                    .options()
                    .connect_timeout
                    .and_then(|timeout| now.checked_add(timeout));
                let inactivity_deadline = request
                    .options()
                    .inactivity_timeout
                    .and_then(|timeout| now.checked_add(timeout));
                match self.make_pending(
                    request_id,
                    request,
                    PendingDeadlines {
                        connect: connect_deadline,
                        total: total_deadline,
                        inactivity: inactivity_deadline,
                    },
                    next_redirect_hops,
                    ErrorKind::Redirect,
                    PendingResponse::Buffered,
                ) {
                    Ok(pending) => {
                        if let Some(completion) = self.start_pending(pending) {
                            completions.push(BackendCompletion {
                                id: request_id,
                                completion,
                            });
                        }
                    }
                    Err((error, _response)) => completions.push(BackendCompletion {
                        id: request_id,
                        completion: Completion::Failed(error),
                    }),
                }
            }
        }
    }

    fn begin_connection(
        &mut self,
        address: SocketAddr,
        mut pending: PendingResolve,
    ) -> Option<Completion> {
        let deadline = pending.next_deadline();
        let tls = if pending.scheme == "https" {
            let Some(configs) = &self.tls else {
                self.release_connection(&pending.key);
                return pending.fail(Error::new(
                    ErrorKind::Unsupported,
                    "the selected native proving Engine has no TLS configuration",
                ));
            };
            match configs.connection(
                &pending.host,
                pending.tls_verification,
                pending.serialized.bytes.clone(),
            ) {
                Ok(tls) => Some(tls),
                Err(error) => {
                    self.release_connection(&pending.key);
                    return pending.fail(error);
                }
            }
        } else {
            None
        };
        let outbound_limit = if tls.is_some() {
            encrypted_outbound_limit()
        } else {
            cleartext_outbound_limit(&pending.serialized, pending.response.upload())
        };
        let receive_limit = if tls.is_some() {
            encrypted_receive_limit(self.limits.reactor_receive_limit())
        } else {
            self.limits.reactor_receive_limit()
        };
        let slot = match self
            .reactor
            .connect(address, deadline, outbound_limit, receive_limit)
        {
            Ok(slot) => slot,
            Err(failure) => {
                self.release_connection(&pending.key);
                return pending.fail(native_transport_error(failure));
            }
        };
        if tls.is_none() {
            if let Err(failure) = self.reactor.queue_write(slot, &pending.serialized.bytes) {
                self.reactor.cancel(slot);
                self.release_connection(&pending.key);
                return pending.fail(native_transport_error(failure));
            }
        }
        let request_id = pending.request_id;
        let now = Instant::now();
        pending.inactivity_deadline = pending
            .inactivity_timeout
            .and_then(|timeout| now.checked_add(timeout));
        let (response, upload) = pending
            .response
            .into_active(pending.serialized.response_to_head, self.limits);
        self.request_to_slot.insert(request_id, slot);
        self.transfers.insert(
            slot,
            HttpTransfer {
                request_id,
                response,
                body_bearing: pending.body_bearing,
                response_started: false,
                connected: false,
                tls,
                connect_deadline: pending.connect_deadline,
                total_deadline: pending.total_deadline,
                inactivity_timeout: pending.inactivity_timeout,
                inactivity_deadline: pending.inactivity_deadline,
                inactivity_paused: false,
                key: pending.key,
                resolver_config_generation: self.resolver_config_generation,
                request_permits_reuse: pending.serialized.permits_reuse,
                request_write_drained: false,
                request: pending.request,
                redirect_hops: pending.redirect_hops,
                upload,
                upload_aborted: false,
            },
        );
        None
    }

    fn process_resolver_results(&mut self) -> Result<Vec<BackendCompletion>, Error> {
        self.apply_idle_http_eviction()?;
        let results = match &self.resolver {
            Some(resolver) => resolver.drain()?,
            None => return Ok(Vec::new()),
        };
        let mut completions = Vec::new();
        for result in results {
            if let Some(public) = result.public {
                if let Some(lookup) = self.standalone_resolves.remove(&result.key) {
                    self.standalone_request_to_resolve.remove(&lookup.sink.id());
                    self.finish_standalone_resolve(lookup, public);
                    continue;
                }
                #[cfg(feature = "resolver")]
                {
                    let Some(lookup) = self.public_lookups.remove(&result.key) else {
                        continue;
                    };
                    self.request_to_public.remove(&lookup.request_id);
                    if lookup
                        .total_deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                    {
                        if let Some(resolver) = &self.resolver {
                            resolver.cancel(result.key)?;
                        }
                        self.pending_public.push(BackendResolveCompletion {
                            id: lookup.request_id,
                            completion: public_total_timeout(),
                        });
                        continue;
                    }
                    self.pending_public.push(BackendResolveCompletion {
                        id: lookup.request_id,
                        completion: match public {
                            PublicLookupOutcome::Completed {
                                name,
                                status,
                                addresses,
                                valid_until,
                                from_cache,
                                candidate_name,
                            } => ResolveCompletion::Completed(ResolveResponse::new(
                                name,
                                status,
                                addresses.into_iter().map(ResolvedAddress::new).collect(),
                                valid_until,
                                from_cache,
                                candidate_name,
                            )),
                            PublicLookupOutcome::Failed(error) => ResolveCompletion::Failed(error),
                        },
                    });
                    continue;
                }
                #[cfg(not(feature = "resolver"))]
                continue;
            }
            let Some(pending) = self.resolves.remove(&result.key) else {
                continue;
            };
            self.request_to_resolve.remove(&pending.request_id);
            match result.result {
                Ok(answer) => {
                    let Some(ip) = answer.addresses.into_iter().next() else {
                        self.release_connection(&pending.key);
                        let id = pending.request_id;
                        if let Some(completion) = pending.fail(Error::transport(
                            TransportStage::Dns,
                            "the native resolver returned no usable address",
                        )) {
                            completions.push(BackendCompletion { id, completion });
                        }
                        continue;
                    };
                    let id = pending.request_id;
                    if let Some(completion) =
                        self.begin_connection(SocketAddr::new(ip, pending.port), pending)
                    {
                        completions.push(BackendCompletion { id, completion });
                    }
                }
                Err(failure) => {
                    self.release_connection(&pending.key);
                    let id = pending.request_id;
                    if let Some(completion) =
                        pending.fail(Error::transport(TransportStage::Dns, failure.message))
                    {
                        completions.push(BackendCompletion { id, completion });
                    }
                }
            }
        }
        Ok(completions)
    }

    fn finish_standalone_resolve(
        &mut self,
        lookup: StandaloneResolve,
        outcome: PublicLookupOutcome,
    ) {
        if lookup
            .connect_deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            lookup.sink.fail(standalone_connect_timeout());
            return;
        }
        if !lookup.sink.release_dns_borrow() {
            return;
        }
        #[cfg(test)]
        if let Some((entered, release)) = self.standalone_dns_handoff_gate.take() {
            entered.send(()).expect("observe standalone DNS handoff");
            release.recv().expect("release standalone DNS handoff");
        }
        if lookup.sink.is_terminal() {
            return;
        }
        match outcome {
            PublicLookupOutcome::Completed {
                status: ResolveStatus::Answer,
                addresses,
                ..
            } if !addresses.is_empty() => {
                let remaining = addresses
                    .into_iter()
                    .map(|address| SocketAddr::new(address, lookup.port))
                    .collect();
                self.start_standalone_attempt(StandalonePending {
                    sink: lookup.sink,
                    remaining,
                    connect_deadline: lookup.connect_deadline,
                });
            }
            PublicLookupOutcome::Completed { status, .. } => {
                let message = match status {
                    ResolveStatus::NameNotFound => "the standalone TCP hostname does not exist",
                    ResolveStatus::NoData => {
                        "the standalone TCP hostname has no usable address data"
                    }
                    ResolveStatus::Answer => {
                        "the standalone TCP hostname resolved without a usable address"
                    }
                };
                lookup
                    .sink
                    .fail(Error::transport(TransportStage::Dns, message));
            }
            PublicLookupOutcome::Failed(error) => {
                lookup.sink.fail(error);
            }
        }
    }

    #[cfg(feature = "resolver")]
    fn expire_public_lookups(&mut self) -> Result<(), Error> {
        let now = Instant::now();
        let expired = self
            .public_lookups
            .iter()
            .filter_map(|(key, lookup)| {
                lookup
                    .total_deadline
                    .is_some_and(|deadline| deadline <= now)
                    .then_some(*key)
            })
            .collect::<Vec<_>>();
        for key in expired {
            let Some(lookup) = self.public_lookups.remove(&key) else {
                continue;
            };
            self.request_to_public.remove(&lookup.request_id);
            if let Some(resolver) = &self.resolver {
                resolver.cancel(key)?;
            }
            self.pending_public.push(BackendResolveCompletion {
                id: lookup.request_id,
                completion: public_total_timeout(),
            });
        }
        Ok(())
    }

    fn expire_standalone_resolves(&mut self) -> Result<(), Error> {
        let now = Instant::now();
        let finished = self
            .standalone_resolves
            .iter()
            .filter_map(|(key, lookup)| {
                (lookup.sink.is_terminal()
                    || lookup
                        .connect_deadline
                        .is_some_and(|deadline| deadline <= now))
                .then_some(*key)
            })
            .collect::<Vec<_>>();
        for key in finished {
            let Some(lookup) = self.standalone_resolves.remove(&key) else {
                continue;
            };
            self.standalone_request_to_resolve.remove(&lookup.sink.id());
            if let Some(resolver) = &self.resolver {
                resolver.cancel(key)?;
            }
            if !lookup.sink.is_terminal() {
                lookup.sink.fail(standalone_connect_timeout());
            }
        }
        Ok(())
    }

    fn drain_dns(&mut self) -> Result<(), Error> {
        #[cfg(feature = "resolver")]
        self.expire_public_lookups()?;
        self.expire_standalone_resolves()?;
        let http = self.process_resolver_results()?;
        self.pending_http_from_dns.extend(http);
        Ok(())
    }

    fn expire_resolves(&mut self) -> Result<Vec<BackendCompletion>, Error> {
        let now = Instant::now();
        let expired = self
            .resolves
            .iter()
            .filter_map(|(key, pending)| {
                pending
                    .next_deadline()
                    .is_some_and(|deadline| deadline <= now)
                    .then_some(*key)
            })
            .collect::<Vec<_>>();
        let mut completions = Vec::new();
        for key in expired {
            let Some(pending) = self.resolves.remove(&key) else {
                continue;
            };
            self.request_to_resolve.remove(&pending.request_id);
            self.release_connection(&pending.key);
            if let Some(resolver) = &self.resolver {
                resolver.cancel(key)?;
            }
            let timeout = pending.expired_timeout(now);
            let id = pending.request_id;
            if let Some(completion) = pending
                .response
                .fail(Error::timeout(timeout, native_timeout_message(timeout)))
            {
                completions.push(BackendCompletion { id, completion });
            }
        }
        Ok(completions)
    }

    fn next_resolve_key(&mut self) -> Result<ResolveKey, Error> {
        let key = ResolveKey(self.next_resolve_key);
        self.next_resolve_key = self.next_resolve_key.checked_add(1).ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                "the native resolver request identity space is exhausted",
            )
        })?;
        Ok(key)
    }

    fn start_standalone_attempt(&mut self, mut pending: StandalonePending) {
        loop {
            if pending.is_terminal() {
                return;
            }
            if pending
                .connect_deadline
                .is_some_and(|deadline| deadline <= Instant::now())
            {
                pending.sink.fail(standalone_connect_timeout());
                return;
            }
            let Some(address) = pending.remaining.pop_front() else {
                pending.sink.fail(Error::transport(
                    TransportStage::Connect,
                    "every resolved standalone TCP address failed to connect",
                ));
                return;
            };
            #[cfg(test)]
            if self.failed_standalone_addresses_remaining != 0 {
                self.failed_standalone_addresses_remaining -= 1;
                if let Some(delay) = self.failed_standalone_address_delay.take() {
                    std::thread::sleep(delay);
                }
                if pending.remaining.is_empty() {
                    pending.sink.fail(Error::transport(
                        TransportStage::Connect,
                        "the injected standalone TCP address failed to connect",
                    ));
                    return;
                }
                continue;
            }
            let slot = match self.reactor.connect(
                address,
                pending.connect_deadline,
                pending.sink.send_window(),
                usize::MAX,
            ) {
                Ok(slot) => slot,
                Err(failure)
                    if failure.kind == NativeFailureKind::Connect
                        && !pending.remaining.is_empty() =>
                {
                    continue;
                }
                Err(failure) => {
                    pending.sink.fail(native_transport_error(failure));
                    return;
                }
            };
            if let Err(failure) = self
                .reactor
                .set_read_allowance(slot, Some(pending.sink.receive_window()))
            {
                self.reactor.cancel(slot);
                pending.sink.fail(native_internal_error(failure));
                return;
            }
            if pending.is_terminal() {
                self.reactor.cancel(slot);
                return;
            }
            self.standalone_request_to_slot.insert(pending.id(), slot);
            self.standalone_pending.insert(slot, pending);
            #[cfg(test)]
            if let Some((entered, release)) = self.standalone_socket_gate.take() {
                entered
                    .send(())
                    .expect("observe owned standalone socket attempt");
                release
                    .recv()
                    .expect("release owned standalone socket attempt");
            }
            return;
        }
    }

    fn start_reserved(&mut self, pending: PendingResolve) -> Option<Completion> {
        if let Ok(ip) = pending.host.parse::<IpAddr>() {
            return self.begin_connection(SocketAddr::new(ip, pending.port), pending);
        }
        let key = match self.next_resolve_key() {
            Ok(key) => key,
            Err(error) => {
                self.release_connection(&pending.key);
                return pending.fail(error);
            }
        };
        let Some(resolver) = &self.resolver else {
            self.release_connection(&pending.key);
            return pending.fail(Error::new(
                ErrorKind::Unsupported,
                "the native HTTP proving Engine requires an injected resolver for hostnames",
            ));
        };
        if let Err(error) = resolver.resolve(key, pending.host.clone()) {
            self.release_connection(&pending.key);
            return pending.fail(error);
        }
        self.request_to_resolve.insert(pending.request_id, key);
        self.resolves.insert(key, pending);
        None
    }

    fn fail_native(
        &mut self,
        slot: SlotId,
        failure: NativeFailure,
        completions: &mut Vec<BackendCompletion>,
    ) {
        let tls_handshake = self
            .transfers
            .get(&slot)
            .and_then(|transfer| transfer.tls.as_ref())
            .is_some_and(NativeTls::is_handshaking);
        let error = if tls_handshake {
            Error::tls(
                TransportStage::Tls,
                TlsFailure::Io,
                format!(
                    "the connection failed during the TLS handshake: {}",
                    failure.message
                ),
            )
        } else if failure.kind == NativeFailureKind::Read
            && self
                .transfers
                .get(&slot)
                .is_some_and(|transfer| transfer.body_bearing && !transfer.response_started)
        {
            Error::transport(
                TransportStage::Send,
                "the connection failed during a body-bearing exchange before any response bytes arrived",
            )
        } else {
            native_transport_error(failure)
        };
        self.finish(slot, Completion::Failed(error), completions);
    }

    fn pump_tls_request(
        &mut self,
        slot: SlotId,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<bool, Error> {
        let capacity = match self.reactor.outbound_capacity(slot) {
            Ok(capacity) => capacity,
            Err(failure) => {
                self.finish(
                    slot,
                    Completion::Failed(Error::new(ErrorKind::Internal, failure.message)),
                    completions,
                );
                return Ok(false);
            }
        };
        let pumped = self
            .transfers
            .get_mut(&slot)
            .and_then(|transfer| transfer.tls.as_mut())
            .map(|tls| tls.pump_request(capacity));
        let outbound = match pumped {
            Some(Ok(outbound)) => outbound,
            Some(Err(error)) => {
                self.finish(slot, Completion::Failed(error), completions);
                return Ok(false);
            }
            None => return Ok(false),
        };
        let queued = !outbound.is_empty();
        if queued {
            if let Err(failure) = self.reactor.queue_write(slot, &outbound) {
                self.finish(
                    slot,
                    Completion::Failed(Error::transport(TransportStage::Send, failure.message)),
                    completions,
                );
                return Ok(false);
            }
        }
        Ok(queued)
    }

    fn pump_request_output(
        &mut self,
        slot: SlotId,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<bool, Error> {
        let mut queued = self.pump_tls_request(slot, completions)?;
        loop {
            let Some(uses_tls) = self
                .transfers
                .get(&slot)
                .map(|transfer| transfer.tls.is_some())
            else {
                return Ok(queued);
            };
            if self
                .transfers
                .get(&slot)
                .is_none_or(|transfer| transfer.upload.is_none())
            {
                return Ok(queued);
            }

            if uses_tls {
                let ready = self
                    .transfers
                    .get(&slot)
                    .and_then(|transfer| transfer.tls.as_ref())
                    .is_some_and(|tls| !tls.is_handshaking() && tls.request_fully_encrypted());
                if !ready {
                    return Ok(queued);
                }
                let next = self
                    .transfers
                    .get_mut(&slot)
                    .and_then(|transfer| transfer.upload.as_mut())
                    .expect("streamed upload presence checked")
                    .next_wire(None);
                match next {
                    Ok(UploadWire::Bytes(wire)) => {
                        let begun = self
                            .transfers
                            .get_mut(&slot)
                            .and_then(|transfer| transfer.tls.as_mut())
                            .expect("TLS upload connection checked")
                            .begin_request(wire);
                        if let Err(error) = begun {
                            self.finish(slot, Completion::Failed(error), completions);
                            return Ok(queued);
                        }
                        queued |= self.pump_tls_request(slot, completions)?;
                    }
                    Ok(UploadWire::Pending | UploadWire::Complete) => return Ok(queued),
                    Err(error) => {
                        self.finish(slot, Completion::Failed(error), completions);
                        return Ok(queued);
                    }
                }
            } else {
                let capacity = match self.reactor.outbound_capacity(slot) {
                    Ok(capacity) => capacity,
                    Err(failure) => {
                        self.finish(
                            slot,
                            Completion::Failed(Error::new(ErrorKind::Internal, failure.message)),
                            completions,
                        );
                        return Ok(queued);
                    }
                };
                let next = self
                    .transfers
                    .get_mut(&slot)
                    .and_then(|transfer| transfer.upload.as_mut())
                    .expect("streamed upload presence checked")
                    .next_wire(Some(capacity));
                match next {
                    Ok(UploadWire::Bytes(wire)) => {
                        if let Err(failure) = self.reactor.queue_write(slot, &wire) {
                            self.finish(
                                slot,
                                Completion::Failed(Error::transport(
                                    TransportStage::Send,
                                    failure.message,
                                )),
                                completions,
                            );
                            return Ok(queued);
                        }
                        queued = true;
                    }
                    Ok(UploadWire::Pending | UploadWire::Complete) => return Ok(queued),
                    Err(error) => {
                        self.finish(slot, Completion::Failed(error), completions);
                        return Ok(queued);
                    }
                }
            }
        }
    }

    fn request_output_complete(transfer: &HttpTransfer) -> bool {
        if transfer.upload_aborted {
            return false;
        }
        let upload_complete = transfer
            .upload
            .as_ref()
            .is_none_or(NativeUpload::is_complete);
        let encrypted = transfer
            .tls
            .as_ref()
            .is_none_or(NativeTls::request_fully_encrypted);
        upload_complete && encrypted
    }

    fn refresh_request_write_drained(&mut self, slot: SlotId) -> Result<(), Error> {
        let complete = self
            .transfers
            .get(&slot)
            .is_some_and(|transfer| transfer.connected && Self::request_output_complete(transfer));
        if !complete {
            return Ok(());
        }
        if self
            .reactor
            .outbound_is_empty(slot)
            .map_err(native_internal_error)?
        {
            if let Some(transfer) = self.transfers.get_mut(&slot) {
                transfer.request_write_drained = true;
            }
        }
        Ok(())
    }

    fn handle_connected(
        &mut self,
        slot: SlotId,
        arm_deadline: bool,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<(), Error> {
        let start = self
            .transfers
            .get_mut(&slot)
            .and_then(|transfer| transfer.tls.as_mut())
            .map(NativeTls::start);
        match start {
            Some(Ok(outbound)) => {
                self.note_progress(slot, false, arm_deadline)?;
                if arm_deadline && !outbound.is_empty() {
                    if let Err(failure) = self.reactor.queue_write(slot, &outbound) {
                        self.finish(
                            slot,
                            Completion::Failed(Error::tls(
                                TransportStage::Tls,
                                TlsFailure::Io,
                                format!(
                                    "native TLS ClientHello queueing failed: {}",
                                    failure.message
                                ),
                            )),
                            completions,
                        );
                    }
                }
            }
            Some(Err(error)) => {
                self.finish(slot, Completion::Failed(error), completions);
            }
            None => self.note_progress(slot, true, arm_deadline)?,
        }
        Ok(())
    }

    fn consume_stream_plaintext(transfer: &mut HttpTransfer, consumed: usize) -> Result<(), Error> {
        match &mut transfer.response {
            TransferResponse::Streaming {
                retained_cleartext,
                retained_offset,
                ..
            } => {
                if let Some(tls) = transfer.tls.as_mut() {
                    tls.consume_retained_plaintext(consumed)
                } else {
                    let remaining = retained_cleartext.len().saturating_sub(*retained_offset);
                    if consumed > remaining {
                        return Err(Error::new(
                            ErrorKind::Internal,
                            "native HTTP consumed beyond retained cleartext streaming bytes",
                        ));
                    }
                    *retained_offset += consumed;
                    if *retained_offset == retained_cleartext.len() {
                        retained_cleartext.clear();
                        *retained_offset = 0;
                    }
                    Ok(())
                }
            }
            TransferResponse::Buffered(_) => Err(Error::new(
                ErrorKind::Internal,
                "buffered native response entered streaming plaintext consumption",
            )),
        }
    }

    fn stream_plaintext(transfer: &HttpTransfer) -> &[u8] {
        match &transfer.response {
            TransferResponse::Streaming {
                retained_cleartext,
                retained_offset,
                ..
            } => transfer.tls.as_ref().map_or_else(
                || &retained_cleartext[*retained_offset..],
                NativeTls::retained_plaintext,
            ),
            TransferResponse::Buffered(_) => &[],
        }
    }

    fn refresh_stream_allowance(&mut self, slot: SlotId) -> Result<(), Error> {
        let stream_state = self.transfers.get(&slot).and_then(|transfer| {
            let TransferResponse::Streaming {
                decoder,
                retained_cleartext,
                retained_offset,
                ..
            } = &transfer.response
            else {
                return None;
            };
            let retained = transfer.tls.as_ref().map_or_else(
                || retained_cleartext.len().saturating_sub(*retained_offset),
                |tls| tls.retained_plaintext().len(),
            );
            let capacity = decoder.socket_capacity();
            let allowance = if let Some(tls) = &transfer.tls {
                tls.streaming_read_allowance(capacity)
            } else if retained != 0 {
                0
            } else {
                capacity
            };
            Some((allowance, decoder.is_consumer_blocked()))
        });
        if let Some((allowance, consumer_blocked)) = stream_state {
            let deadline_update = self.transfers.get_mut(&slot).and_then(|transfer| {
                let mut changed = false;
                if consumer_blocked && !transfer.inactivity_paused {
                    transfer.inactivity_paused = true;
                    transfer.inactivity_deadline = None;
                    changed = true;
                } else if !consumer_blocked && transfer.inactivity_paused {
                    transfer.inactivity_paused = false;
                    transfer.inactivity_deadline = transfer
                        .inactivity_timeout
                        .and_then(|timeout| Instant::now().checked_add(timeout));
                    changed = true;
                }
                changed.then(|| transfer.next_deadline())
            });
            if let Some(deadline) = deadline_update {
                self.reactor
                    .set_deadline(slot, deadline)
                    .map_err(native_internal_error)?;
            }
            self.reactor
                .set_read_allowance(slot, Some(allowance))
                .map_err(native_internal_error)?;
        }
        Ok(())
    }

    fn complete_stream_response(
        &mut self,
        slot: SlotId,
        response_permits_reuse: bool,
        peer_closed: bool,
    ) {
        let Some(mut transfer) = self.transfers.remove(&slot) else {
            self.reactor.cancel(slot);
            return;
        };
        self.request_to_slot.remove(&transfer.request_id);
        let request_id = transfer.request_id;
        let total_deadline = transfer.total_deadline;
        let next_redirect_hops = transfer.redirect_hops.saturating_add(1);
        let mut redirect_transfer_failed = false;
        let redirect = match &mut transfer.response {
            TransferResponse::Streaming {
                decoder, redirect, ..
            } if redirect.is_some() => {
                let request = redirect.take().expect("redirect presence checked");
                match decoder.take_response() {
                    Ok(response) => Some((request, response)),
                    Err(error) => {
                        redirect_transfer_failed = true;
                        decoder.fail(error);
                        None
                    }
                }
            }
            TransferResponse::Streaming { .. } => None,
            TransferResponse::Buffered(_) => {
                debug_assert!(false, "buffered response entered streaming completion");
                None
            }
        };
        let per_origin_idle = self.idle.get(&transfer.key).map_or(0, VecDeque::len);
        let reusable = response_permits_reuse
            && !redirect_transfer_failed
            && transfer.request_permits_reuse
            && transfer.request_write_drained
            && transfer.resolver_config_generation == self.resolver_config_generation
            && !peer_closed
            && self.connection_limits.idle_timeout != Duration::ZERO
            && self.idle_count < self.connection_limits.idle_global
            && per_origin_idle < self.connection_limits.idle_per_origin;
        if reusable {
            let parked = Instant::now()
                .checked_add(self.connection_limits.idle_timeout)
                .filter(|idle_deadline| {
                    self.reactor
                        .set_deadline(slot, Some(*idle_deadline))
                        .is_ok()
                });
            if let Some(expires_at) = parked {
                self.idle_slots.insert(slot, transfer.key.clone());
                self.idle
                    .entry(transfer.key)
                    .or_default()
                    .push_back(IdleConnection {
                        slot,
                        tls: transfer.tls.take(),
                        expires_at,
                    });
                self.idle_count += 1;
                if let Some(metrics) = &self.metrics {
                    metrics.set_idle_connections(self.idle_count);
                }
            } else {
                self.reactor.cancel(slot);
                self.release_connection(&transfer.key);
            }
        } else {
            self.reactor.cancel(slot);
            self.release_connection(&transfer.key);
        }

        if let Some((request, response)) = redirect {
            let now = Instant::now();
            let deadlines = PendingDeadlines {
                connect: request
                    .options()
                    .connect_timeout
                    .and_then(|timeout| now.checked_add(timeout)),
                total: total_deadline,
                inactivity: request
                    .options()
                    .inactivity_timeout
                    .and_then(|timeout| now.checked_add(timeout)),
            };
            match self.make_pending(
                request_id,
                request,
                deadlines,
                next_redirect_hops,
                ErrorKind::Redirect,
                PendingResponse::Streaming {
                    response,
                    upload: None,
                },
            ) {
                Ok(pending) => {
                    let completion = self.start_pending(pending);
                    debug_assert!(completion.is_none());
                }
                Err((
                    error,
                    PendingResponse::Streaming {
                        mut response,
                        upload: _,
                    },
                )) => response.fail(error),
                Err((_error, PendingResponse::Buffered)) => {
                    unreachable!("stream redirect changed pending response kind")
                }
            }
        }
    }

    fn drain_stream_plaintext(
        &mut self,
        slot: SlotId,
        peer_closed: bool,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<(), Error> {
        if peer_closed {
            if let Some(HttpTransfer {
                response: TransferResponse::Streaming { peer_closed, .. },
                ..
            }) = self.transfers.get_mut(&slot)
            {
                *peer_closed = true;
            }
        }
        loop {
            let progress = {
                let Some(transfer) = self.transfers.get_mut(&slot) else {
                    return Ok(());
                };
                let HttpTransfer {
                    response,
                    response_started,
                    tls,
                    ..
                } = transfer;
                let TransferResponse::Streaming {
                    decoder,
                    retained_cleartext,
                    retained_offset,
                    ..
                } = response
                else {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "buffered native response entered streaming decoder drain",
                    ));
                };
                let plaintext = tls.as_ref().map_or_else(
                    || &retained_cleartext[*retained_offset..],
                    NativeTls::retained_plaintext,
                );
                if plaintext.is_empty() {
                    None
                } else {
                    *response_started = true;
                    Some(decoder.ingest(plaintext))
                }
            };

            let Some(progress) = progress else {
                break;
            };
            let progress = match progress {
                Ok(progress) => progress,
                Err(error) => {
                    self.finish(slot, Completion::Failed(error), completions);
                    return Ok(());
                }
            };
            match progress {
                StreamDecodeProgress::Head { head, consumed } => {
                    let redirect = self.transfers.get(&slot).map(|transfer| {
                        if transfer.upload.is_some() {
                            Ok(None)
                        } else {
                            redirected_request(
                                &transfer.request,
                                head.status(),
                                transfer.redirect_hops,
                                || {
                                    resolved_redirect_target_from_headers(
                                        &transfer.request,
                                        head.headers(),
                                    )
                                },
                            )
                        }
                    });
                    let (deliver, redirect) = match redirect {
                        Some(Ok(Some(request))) => (false, Some(request)),
                        Some(Ok(None)) => (true, None),
                        Some(Err(error)) => {
                            self.finish(slot, Completion::Failed(error), completions);
                            return Ok(());
                        }
                        None => return Ok(()),
                    };
                    let decision = if let Some(transfer) = self.transfers.get_mut(&slot) {
                        Self::consume_stream_plaintext(transfer, consumed)?;
                        if let Some(mut upload) = transfer.upload.take() {
                            transfer.upload_aborted |=
                                !upload.is_complete() || !transfer.request_write_drained;
                            upload.close();
                        }
                        let TransferResponse::Streaming {
                            decoder,
                            redirect: pending_redirect,
                            ..
                        } = &mut transfer.response
                        else {
                            unreachable!("streaming transfer changed response kind")
                        };
                        *pending_redirect = redirect;
                        decoder.decide_head(deliver)
                    } else {
                        return Ok(());
                    };
                    match decision {
                        Ok(StreamHeadDecision {
                            complete: true,
                            permits_reuse,
                        }) => {
                            let trailing = self.transfers.get(&slot).is_some_and(|transfer| {
                                !Self::stream_plaintext(transfer).is_empty()
                            });
                            let redirecting = self.transfers.get(&slot).is_some_and(|transfer| {
                                matches!(
                                    transfer.response,
                                    TransferResponse::Streaming {
                                        redirect: Some(_),
                                        ..
                                    }
                                )
                            });
                            if trailing && redirecting {
                                self.finish(
                                    slot,
                                    Completion::Failed(Error::transport(
                                        TransportStage::Http,
                                        "the peer sent bytes after a no-body redirect response",
                                    )),
                                    completions,
                                );
                                return Ok(());
                            }
                            let peer_closed = self.transfers.get(&slot).is_some_and(|transfer| {
                                matches!(
                                    transfer.response,
                                    TransferResponse::Streaming {
                                        peer_closed: true,
                                        ..
                                    }
                                )
                            });
                            self.complete_stream_response(
                                slot,
                                permits_reuse && !trailing,
                                peer_closed,
                            );
                            return Ok(());
                        }
                        Ok(StreamHeadDecision {
                            complete: false, ..
                        }) => {}
                        Err(error) => {
                            self.finish(slot, Completion::Failed(error), completions);
                            return Ok(());
                        }
                    }
                }
                StreamDecodeProgress::Body { consumed, blocked } => {
                    if let Some(transfer) = self.transfers.get_mut(&slot) {
                        Self::consume_stream_plaintext(transfer, consumed)?;
                    }
                    if blocked || consumed == 0 {
                        break;
                    }
                }
                StreamDecodeProgress::Complete {
                    consumed,
                    permits_reuse,
                    ..
                } => {
                    if let Some(transfer) = self.transfers.get_mut(&slot) {
                        Self::consume_stream_plaintext(transfer, consumed)?;
                    }
                    let trailing = self
                        .transfers
                        .get(&slot)
                        .is_some_and(|transfer| !Self::stream_plaintext(transfer).is_empty());
                    if trailing {
                        self.finish(
                            slot,
                            Completion::Failed(Error::transport(
                                TransportStage::Http,
                                "the peer sent bytes after the completed streaming HTTP response",
                            )),
                            completions,
                        );
                    } else {
                        let peer_closed = self.transfers.get(&slot).is_some_and(|transfer| {
                            matches!(
                                transfer.response,
                                TransferResponse::Streaming {
                                    peer_closed: true,
                                    ..
                                }
                            )
                        });
                        self.complete_stream_response(slot, permits_reuse, peer_closed);
                    }
                    return Ok(());
                }
            }
        }

        let should_eof = self.transfers.get(&slot).is_some_and(|transfer| {
            matches!(
                transfer.response,
                TransferResponse::Streaming {
                    peer_closed: true,
                    ..
                }
            ) && Self::stream_plaintext(transfer).is_empty()
        });
        if should_eof {
            let tls_dirty_eof = self.transfers.get(&slot).is_some_and(|transfer| {
                matches!(
                    transfer.response,
                    TransferResponse::Streaming {
                        tls_dirty_eof: true,
                        ..
                    }
                )
            });
            if tls_dirty_eof {
                self.finish(
                    slot,
                    Completion::Failed(Error::transport(
                        TransportStage::Receive,
                        "the TLS peer closed without an authenticated close notification",
                    )),
                    completions,
                );
                return Ok(());
            }
            let eof = self.transfers.get_mut(&slot).map(|transfer| {
                let TransferResponse::Streaming { decoder, .. } = &mut transfer.response else {
                    unreachable!("streaming transfer changed response kind")
                };
                decoder.eof()
            });
            match eof {
                Some(Ok(Some((permits_reuse, _)))) => {
                    self.complete_stream_response(slot, permits_reuse, true)
                }
                Some(Ok(None)) => self.finish(
                    slot,
                    Completion::Failed(Error::transport(
                        TransportStage::Receive,
                        "the peer closed without completing a streaming response",
                    )),
                    completions,
                ),
                Some(Err(error)) => self.finish(slot, Completion::Failed(error), completions),
                None => {}
            }
        }
        if self.transfers.contains_key(&slot) {
            self.refresh_stream_allowance(slot)?;
        }
        Ok(())
    }

    fn handle_stream_data(
        &mut self,
        slot: SlotId,
        bytes: Vec<u8>,
        arm_deadline: bool,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<(), Error> {
        let tls_progress = self
            .transfers
            .get_mut(&slot)
            .and_then(|transfer| transfer.tls.as_mut())
            .map(|tls| tls.receive_streaming(&bytes));
        let (established, outbound, peer_closed) = match tls_progress {
            Some(Ok(TlsStreamProgress {
                outbound,
                handshake_complete,
                peer_closed,
            })) => (handshake_complete, outbound, peer_closed),
            Some(Err(error)) => {
                self.finish(slot, Completion::Failed(error), completions);
                return Ok(());
            }
            None => {
                let Some(transfer) = self.transfers.get_mut(&slot) else {
                    return Ok(());
                };
                let TransferResponse::Streaming {
                    retained_cleartext,
                    retained_offset,
                    ..
                } = &mut transfer.response
                else {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "buffered native response entered streaming data path",
                    ));
                };
                if *retained_offset != retained_cleartext.len() {
                    self.finish(
                        slot,
                        Completion::Failed(Error::new(
                            ErrorKind::Internal,
                            "native cleartext accepted bytes while streaming plaintext remained",
                        )),
                        completions,
                    );
                    return Ok(());
                }
                *retained_cleartext = bytes;
                *retained_offset = 0;
                (false, Vec::new(), false)
            }
        };
        self.note_progress(slot, established, arm_deadline)?;
        if arm_deadline && !outbound.is_empty() {
            if let Err(failure) = self.reactor.queue_write(slot, &outbound) {
                self.finish(
                    slot,
                    Completion::Failed(Error::tls(
                        TransportStage::Tls,
                        TlsFailure::Io,
                        failure.message,
                    )),
                    completions,
                );
                return Ok(());
            }
        }
        if arm_deadline && established && self.transfers.contains_key(&slot) {
            self.pump_request_output(slot, completions)?;
        }
        self.drain_stream_plaintext(slot, peer_closed, completions)
    }

    fn resume_streams(&mut self, completions: &mut Vec<BackendCompletion>) -> Result<(), Error> {
        let slots = self
            .transfers
            .iter()
            .filter_map(|(slot, transfer)| transfer.response.is_streaming().then_some(*slot))
            .collect::<Vec<_>>();
        for slot in slots {
            self.pump_request_output(slot, completions)?;
            if self.transfers.contains_key(&slot) {
                self.refresh_request_write_drained(slot)?;
            }
            self.drain_stream_plaintext(slot, false, completions)?;
            if self.transfers.contains_key(&slot) {
                self.refresh_stream_allowance(slot)?;
            }
        }
        Ok(())
    }

    fn handle_data(
        &mut self,
        slot: SlotId,
        bytes: Vec<u8>,
        arm_deadline: bool,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<(), Error> {
        if self
            .transfers
            .get(&slot)
            .is_some_and(|transfer| transfer.response.is_streaming())
        {
            return self.handle_stream_data(slot, bytes, arm_deadline, completions);
        }
        let tls_progress = self
            .transfers
            .get_mut(&slot)
            .and_then(|transfer| transfer.tls.as_mut())
            .map(|tls| tls.receive(&bytes));
        let (plaintext, established, outbound, peer_closed) = match tls_progress {
            Some(Ok(TlsProgress {
                outbound,
                plaintext,
                handshake_complete,
                peer_closed,
            })) => (plaintext, handshake_complete, outbound, peer_closed),
            Some(Err(error)) => {
                self.finish(slot, Completion::Failed(error), completions);
                return Ok(());
            }
            None => (bytes, false, Vec::new(), false),
        };
        self.note_progress(slot, established, arm_deadline)?;
        if arm_deadline && !outbound.is_empty() {
            if let Err(failure) = self.reactor.queue_write(slot, &outbound) {
                let handshaking = self
                    .transfers
                    .get(&slot)
                    .and_then(|transfer| transfer.tls.as_ref())
                    .is_some_and(NativeTls::is_handshaking);
                let stage = if handshaking {
                    TransportStage::Tls
                } else {
                    TransportStage::Send
                };
                self.finish(
                    slot,
                    Completion::Failed(if stage == TransportStage::Tls {
                        Error::tls(stage, TlsFailure::Io, failure.message)
                    } else {
                        Error::transport(stage, failure.message)
                    }),
                    completions,
                );
                return Ok(());
            }
        }
        if arm_deadline && established && self.transfers.contains_key(&slot) {
            self.pump_request_output(slot, completions)?;
        }
        let decoded = self.transfers.get_mut(&slot).map(|transfer| {
            transfer.response_started |= !plaintext.is_empty();
            match &mut transfer.response {
                TransferResponse::Buffered(decoder) => decoder.ingest(&plaintext),
                TransferResponse::Streaming { .. } => unreachable!("streaming data branched above"),
            }
        });
        match decoded {
            Some(Ok(DecodeProgress {
                response: Some(response),
                consumed,
                permits_reuse,
            })) if consumed == plaintext.len() => {
                self.complete_response(slot, response, permits_reuse, peer_closed, completions)
            }
            Some(Ok(DecodeProgress {
                response: Some(_), ..
            })) => self.finish(
                slot,
                Completion::Failed(Error::transport(
                    TransportStage::Http,
                    "the peer sent bytes after the completed HTTP response",
                )),
                completions,
            ),
            Some(Err(error)) => self.finish(slot, Completion::Failed(error), completions),
            Some(Ok(DecodeProgress { response: None, .. })) if peer_closed => {
                let eof =
                    self.transfers
                        .get_mut(&slot)
                        .map(|transfer| match &mut transfer.response {
                            TransferResponse::Buffered(decoder) => decoder.eof(),
                            TransferResponse::Streaming { .. } => {
                                unreachable!("streaming data branched above")
                            }
                        });
                match eof {
                    Some(Ok(Some(response))) => {
                        self.finish(slot, Completion::Completed(response), completions)
                    }
                    Some(Err(error)) => self.finish(slot, Completion::Failed(error), completions),
                    Some(Ok(None)) => self.finish(
                        slot,
                        Completion::Failed(Error::transport(
                            TransportStage::Receive,
                            "the TLS peer closed without completing a response",
                        )),
                        completions,
                    ),
                    None => {}
                }
            }
            Some(Ok(DecodeProgress { response: None, .. })) | None => {}
        }
        Ok(())
    }

    fn note_progress(
        &mut self,
        slot: SlotId,
        connected: bool,
        arm_deadline: bool,
    ) -> Result<(), Error> {
        let deadline = self
            .transfers
            .get_mut(&slot)
            .map(|transfer| transfer.note_progress(Instant::now(), connected));
        if arm_deadline {
            if let Some(deadline) = deadline {
                self.reactor
                    .set_deadline(slot, deadline)
                    .map_err(native_internal_error)?;
            }
        }
        Ok(())
    }

    fn resume_standalone_tcp(&mut self) -> Result<(), Error> {
        let cancelled = self
            .standalone_pending
            .iter()
            .filter_map(|(slot, sink)| sink.is_terminal().then_some(*slot))
            .collect::<Vec<_>>();
        for slot in cancelled {
            if let Some(sink) = self.standalone_pending.remove(&slot) {
                self.standalone_request_to_slot.remove(&sink.id());
            }
            self.reactor.cancel(slot);
        }

        let slots = self.standalone_live.keys().copied().collect::<Vec<_>>();
        for slot in slots {
            let Some(mut live) = self.standalone_live.remove(&slot) else {
                continue;
            };
            if live.owner.session_released() {
                self.standalone_request_to_slot.remove(&live.request_id);
                self.reactor.cancel(slot);
                continue;
            }
            let result = (|| {
                live.sync_pressure(Instant::now());
                self.reactor
                    .set_read_allowance(slot, Some(live.owner.read_allowance()))
                    .map_err(native_internal_error)?;
                loop {
                    let capacity = self
                        .reactor
                        .outbound_capacity(slot)
                        .map_err(native_internal_error)?;
                    let Some(bytes) = live.owner.take_outbound_up_to(capacity) else {
                        break;
                    };
                    self.reactor
                        .queue_write(slot, &bytes)
                        .map_err(native_internal_error)?;
                }
                if live.owner.finish_requested()
                    && !live.owner.write_finished()
                    && live.owner.send_occupancy() == 0
                    && self
                        .reactor
                        .outbound_is_empty(slot)
                        .map_err(native_internal_error)?
                {
                    self.reactor
                        .shutdown_write(slot)
                        .map_err(native_transport_error)?;
                    live.owner.complete_write_shutdown()?;
                }
                live.sync_pressure(Instant::now());
                self.reactor
                    .set_deadline(slot, live.next_deadline())
                    .map_err(native_internal_error)?;
                Ok::<(), Error>(())
            })();
            if let Err(error) = result {
                live.owner.fail(error);
                self.standalone_request_to_slot.remove(&live.request_id);
                self.reactor.cancel(slot);
            } else {
                self.standalone_live.insert(slot, live);
            }
        }
        Ok(())
    }

    fn handle_standalone_event(
        &mut self,
        event: &NativeEvent,
        terminal_failure_in_batch: bool,
    ) -> Result<bool, Error> {
        let slot = match event {
            NativeEvent::Connected(slot)
            | NativeEvent::WriteProgress(slot, _)
            | NativeEvent::WriteDrained(slot)
            | NativeEvent::Data(slot, _)
            | NativeEvent::PeerClosed(slot)
            | NativeEvent::Failed(slot, _)
            | NativeEvent::DeadlineExpired(slot) => *slot,
        };
        if !self.standalone_pending.contains_key(&slot) && !self.standalone_live.contains_key(&slot)
        {
            return Ok(false);
        }

        match event {
            NativeEvent::Connected(slot) => {
                if terminal_failure_in_batch {
                    return Ok(true);
                }
                let Some(sink) = self.standalone_pending.remove(slot) else {
                    return Ok(true);
                };
                if sink.is_terminal() {
                    self.standalone_request_to_slot.remove(&sink.id());
                    self.reactor.cancel(*slot);
                    return Ok(true);
                }
                let local = self
                    .reactor
                    .local_addr(*slot)
                    .map_err(native_internal_error)?;
                let peer = self
                    .reactor
                    .peer_addr(*slot)
                    .map_err(native_internal_error)?;
                let Some(owner) = sink.connected(local, peer) else {
                    self.standalone_request_to_slot.remove(&sink.id());
                    self.reactor.cancel(*slot);
                    return Ok(true);
                };
                self.reactor
                    .set_deadline(*slot, None)
                    .map_err(native_internal_error)?;
                self.reactor
                    .set_read_allowance(*slot, Some(owner.read_allowance()))
                    .map_err(native_internal_error)?;
                let live = StandaloneTcp::new(
                    sink.id(),
                    owner,
                    sink.read_inactivity_timeout(),
                    sink.write_inactivity_timeout(),
                    Instant::now(),
                );
                self.reactor
                    .set_deadline(*slot, live.next_deadline())
                    .map_err(native_internal_error)?;
                self.standalone_live.insert(*slot, live);
            }
            NativeEvent::WriteProgress(slot, written) => {
                if let Some(live) = self.standalone_live.get_mut(slot) {
                    live.owner.write_progress(*written);
                    live.note_write_progress(Instant::now());
                    if !terminal_failure_in_batch {
                        self.reactor
                            .set_deadline(*slot, live.next_deadline())
                            .map_err(native_internal_error)?;
                    }
                }
            }
            NativeEvent::WriteDrained(slot) => {
                if let Some(live) = self.standalone_live.get_mut(slot) {
                    let progressed = live.owner.pump_bytes();
                    live.owner.write_progress(progressed);
                    live.note_write_progress(Instant::now());
                    if !terminal_failure_in_batch {
                        self.reactor
                            .set_deadline(*slot, live.next_deadline())
                            .map_err(native_internal_error)?;
                    }
                }
            }
            NativeEvent::Data(slot, bytes) => {
                if let Some(live) = self.standalone_live.get_mut(slot) {
                    live.note_read_progress(Instant::now());
                    if let Err(error) = live.owner.push_inbound(bytes.clone()) {
                        live.owner.fail(error);
                    } else {
                        live.sync_pressure(Instant::now());
                        if !terminal_failure_in_batch {
                            self.reactor
                                .set_deadline(*slot, live.next_deadline())
                                .map_err(native_internal_error)?;
                        }
                    }
                }
            }
            NativeEvent::PeerClosed(slot) => {
                if let Some(live) = self.standalone_live.get_mut(slot) {
                    live.owner.peer_closed();
                    live.note_peer_closed();
                    if !terminal_failure_in_batch {
                        self.reactor
                            .set_deadline(*slot, live.next_deadline())
                            .map_err(native_internal_error)?;
                    }
                }
            }
            NativeEvent::Failed(slot, failure) => {
                if let Some(pending) = self.standalone_pending.remove(slot) {
                    self.standalone_request_to_slot.remove(&pending.id());
                    if failure.kind == NativeFailureKind::Connect && !pending.remaining.is_empty() {
                        self.start_standalone_attempt(pending);
                    } else {
                        pending.sink.fail(native_transport_error(failure.clone()));
                    }
                }
                if let Some(mut live) = self.standalone_live.remove(slot) {
                    self.standalone_request_to_slot.remove(&live.request_id);
                    if failure.is_connection_reset() {
                        live.owner.reset();
                    } else {
                        live.owner.fail(native_transport_error(failure.clone()));
                    }
                }
            }
            NativeEvent::DeadlineExpired(slot) => {
                if terminal_failure_in_batch {
                    return Ok(true);
                }
                if let Some(pending) = self.standalone_pending.remove(slot) {
                    self.standalone_request_to_slot.remove(&pending.id());
                    pending.sink.fail(standalone_connect_timeout());
                    self.reactor.cancel(*slot);
                }
                if let Some(mut live) = self.standalone_live.remove(slot) {
                    if let Some(direction) = live.expired(Instant::now()) {
                        self.standalone_request_to_slot.remove(&live.request_id);
                        let message = match direction {
                            StandaloneInactivity::Read => {
                                "the standalone TCP read inactivity timeout expired"
                            }
                            StandaloneInactivity::Write => {
                                "the standalone TCP write inactivity timeout expired"
                            }
                        };
                        live.owner
                            .fail(Error::timeout(TimeoutKind::Inactivity, message));
                        self.reactor.cancel(*slot);
                    } else {
                        self.reactor
                            .set_deadline(*slot, live.next_deadline())
                            .map_err(native_internal_error)?;
                        self.standalone_live.insert(*slot, live);
                    }
                }
            }
        }
        Ok(true)
    }

    fn process_events(
        &mut self,
        events: Vec<NativeEvent>,
    ) -> Result<Vec<BackendCompletion>, Error> {
        // A readiness pass can make write progress and then observe a read-side reset before it
        // returns to the protocol owner. The reactor has already removed that socket by the time
        // this batch is processed, so progress remains meaningful but must not re-arm its deadline.
        let failed_slots = events
            .iter()
            .filter_map(|event| match event {
                NativeEvent::Failed(slot, _) => Some(*slot),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut completions = Vec::new();
        for event in events {
            let event_slot = match &event {
                NativeEvent::Connected(slot)
                | NativeEvent::WriteProgress(slot, _)
                | NativeEvent::WriteDrained(slot)
                | NativeEvent::Data(slot, _)
                | NativeEvent::PeerClosed(slot)
                | NativeEvent::Failed(slot, _)
                | NativeEvent::DeadlineExpired(slot) => *slot,
            };
            if self.handle_standalone_event(&event, failed_slots.contains(&event_slot))? {
                continue;
            }
            if self.idle_slots.contains_key(&event_slot) {
                match event {
                    NativeEvent::WriteProgress(_, _) | NativeEvent::WriteDrained(_) => {}
                    NativeEvent::Connected(_)
                    | NativeEvent::Data(_, _)
                    | NativeEvent::PeerClosed(_)
                    | NativeEvent::Failed(_, _)
                    | NativeEvent::DeadlineExpired(_) => self.discard_idle_slot(event_slot),
                }
                continue;
            }
            match event {
                NativeEvent::Connected(slot) => {
                    self.handle_connected(slot, !failed_slots.contains(&slot), &mut completions)?;
                    if !failed_slots.contains(&slot) && self.transfers.contains_key(&slot) {
                        self.pump_request_output(slot, &mut completions)?;
                    }
                }
                NativeEvent::WriteProgress(slot, _) => {
                    self.note_progress(slot, false, !failed_slots.contains(&slot))?;
                    if !failed_slots.contains(&slot) {
                        self.pump_request_output(slot, &mut completions)?;
                    }
                }
                NativeEvent::WriteDrained(slot) => {
                    self.note_progress(slot, false, !failed_slots.contains(&slot))?;
                    let queued_more = if failed_slots.contains(&slot) {
                        false
                    } else {
                        self.pump_request_output(slot, &mut completions)?
                    };
                    if !queued_more && self.transfers.contains_key(&slot) {
                        self.refresh_request_write_drained(slot)?;
                    }
                }
                NativeEvent::Data(slot, bytes) => {
                    self.handle_data(slot, bytes, !failed_slots.contains(&slot), &mut completions)?;
                }
                NativeEvent::PeerClosed(slot) => {
                    let tls_state = self
                        .transfers
                        .get(&slot)
                        .and_then(|transfer| transfer.tls.as_ref())
                        .map(|tls| tls.is_handshaking());
                    if tls_state == Some(true) {
                        self.finish(
                            slot,
                            Completion::Failed(Error::tls(
                                TransportStage::Tls,
                                TlsFailure::Protocol,
                                "the peer closed during the TLS handshake",
                            )),
                            &mut completions,
                        );
                        continue;
                    }
                    if tls_state == Some(false) {
                        if let Some(HttpTransfer {
                            response:
                                TransferResponse::Streaming {
                                    peer_closed,
                                    tls_dirty_eof,
                                    ..
                                },
                            ..
                        }) = self.transfers.get_mut(&slot)
                        {
                            *peer_closed = true;
                            *tls_dirty_eof = true;
                            self.drain_stream_plaintext(slot, false, &mut completions)?;
                            continue;
                        }
                        self.finish(
                            slot,
                            Completion::Failed(Error::transport(
                                TransportStage::Receive,
                                "the TLS peer closed without an authenticated close notification",
                            )),
                            &mut completions,
                        );
                        continue;
                    }
                    if self
                        .transfers
                        .get(&slot)
                        .is_some_and(|transfer| transfer.response.is_streaming())
                    {
                        self.drain_stream_plaintext(slot, true, &mut completions)?;
                        continue;
                    }
                    let decoded = self
                        .transfers
                        .get_mut(&slot)
                        .map(|transfer| match &mut transfer.response {
                            TransferResponse::Buffered(decoder) => decoder.eof(),
                            TransferResponse::Streaming { .. } => {
                                unreachable!("streaming peer close branched above")
                            }
                        });
                    match decoded {
                        Some(Ok(Some(response))) => {
                            self.finish(slot, Completion::Completed(response), &mut completions)
                        }
                        Some(Ok(None)) => self.finish(
                            slot,
                            Completion::Failed(Error::transport(
                                TransportStage::Receive,
                                "the peer closed without completing a response",
                            )),
                            &mut completions,
                        ),
                        Some(Err(error)) => {
                            self.finish(slot, Completion::Failed(error), &mut completions);
                        }
                        None => {
                            self.reactor.cancel(slot);
                        }
                    }
                }
                NativeEvent::Failed(slot, failure) => {
                    self.fail_native(slot, failure, &mut completions);
                }
                NativeEvent::DeadlineExpired(slot) => {
                    let now = Instant::now();
                    let deadline = self
                        .transfers
                        .get(&slot)
                        .and_then(HttpTransfer::next_deadline);
                    if deadline.is_some_and(|deadline| deadline <= now) {
                        let timeout = self
                            .transfers
                            .get(&slot)
                            .map_or(TimeoutKind::Unknown, |transfer| {
                                transfer.expired_timeout(now)
                            });
                        self.finish(
                            slot,
                            Completion::Failed(Error::timeout(
                                timeout,
                                native_timeout_message(timeout),
                            )),
                            &mut completions,
                        );
                    } else if let Some(deadline) = deadline {
                        self.reactor
                            .set_deadline(slot, Some(deadline))
                            .map_err(native_internal_error)?;
                    }
                }
            }
        }
        Ok(completions)
    }
}

impl Backend for NativeHttpBackend {
    fn connection_metrics_available(&self) -> bool {
        true
    }

    fn attach_metrics(&mut self, metrics: Arc<Metrics>) {
        metrics.set_active_connections(self.connection_count);
        metrics.set_idle_connections(self.idle_count);
        metrics.set_connection_waiters(self.waiting.len());
        self.metrics = Some(metrics);
    }

    fn submit(
        &mut self,
        id: RequestId,
        request: Request,
        accepted_at: Instant,
    ) -> Option<Completion> {
        let options = request.options();
        let connect_deadline = options
            .connect_timeout
            .and_then(|timeout| accepted_at.checked_add(timeout));
        let total_deadline = options
            .total_timeout
            .and_then(|timeout| accepted_at.checked_add(timeout));
        let inactivity_deadline = options
            .inactivity_timeout
            .and_then(|timeout| accepted_at.checked_add(timeout));
        let pending = match self.make_pending(
            id,
            request,
            PendingDeadlines {
                connect: connect_deadline,
                total: total_deadline,
                inactivity: inactivity_deadline,
            },
            0,
            ErrorKind::InvalidRequest,
            PendingResponse::Buffered,
        ) {
            Ok(pending) => pending,
            Err((error, _response)) => return Some(Completion::Failed(error)),
        };
        self.start_pending(pending)
    }

    fn submit_stream(
        &mut self,
        id: RequestId,
        request: StreamRequest,
        response: ResponseSink,
        accepted_at: Instant,
    ) {
        let (request, upload) = request.into_parts();
        let options = request.options();
        let deadlines = PendingDeadlines {
            connect: options
                .connect_timeout
                .and_then(|timeout| accepted_at.checked_add(timeout)),
            total: options
                .total_timeout
                .and_then(|timeout| accepted_at.checked_add(timeout)),
            inactivity: options
                .inactivity_timeout
                .and_then(|timeout| accepted_at.checked_add(timeout)),
        };
        let pending = match self.make_pending(
            id,
            request,
            deadlines,
            0,
            ErrorKind::InvalidRequest,
            PendingResponse::Streaming { response, upload },
        ) {
            Ok(pending) => pending,
            Err((
                error,
                PendingResponse::Streaming {
                    mut response,
                    upload: _,
                },
            )) => {
                response.fail(error);
                return;
            }
            Err((_error, PendingResponse::Buffered)) => {
                unreachable!("stream submission changed pending response kind")
            }
        };
        let completion = self.start_pending(pending);
        debug_assert!(
            completion.is_none(),
            "streaming pending failures must commit directly into ResponseSink"
        );
    }

    fn cancel(&mut self, id: RequestId) {
        if let Some(key) = self.standalone_request_to_resolve.remove(&id) {
            self.standalone_resolves.remove(&key);
            if let Some(resolver) = &self.resolver {
                let _cancel_result = resolver.cancel(key);
            }
            return;
        }
        if let Some(slot) = self.standalone_request_to_slot.remove(&id) {
            self.standalone_pending.remove(&slot);
            if let Some(mut live) = self.standalone_live.remove(&slot) {
                live.owner.cancel();
            }
            self.reactor.cancel(slot);
            return;
        }
        if let Some(index) = self
            .waiting
            .iter()
            .position(|pending| pending.request_id == id)
        {
            self.waiting.remove(index);
            if let Some(metrics) = &self.metrics {
                metrics.set_connection_waiters(self.waiting.len());
            }
        }
        if let Some(key) = self.request_to_resolve.remove(&id) {
            if let Some(pending) = self.resolves.remove(&key) {
                self.release_connection(&pending.key);
            }
            if let Some(resolver) = &self.resolver {
                let _cancel_result = resolver.cancel(key);
            }
        }
        #[cfg(feature = "resolver")]
        {
            if let Some(key) = self.request_to_public.remove(&id) {
                self.public_lookups.remove(&key);
                if let Some(resolver) = &self.resolver {
                    let _cancel_result = resolver.cancel(key);
                }
            }
        }
        if let Some(slot) = self.request_to_slot.remove(&id) {
            if let Some(transfer) = self.transfers.remove(&slot) {
                self.release_connection(&transfer.key);
            }
            self.reactor.cancel(slot);
        }
    }

    fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
        self.apply_idle_http_eviction()?;
        self.resume_standalone_tcp()?;
        self.expire_idle(Instant::now());
        let mut completions = self.dispatch_waiting();
        self.drain_dns()?;
        completions.extend(std::mem::take(&mut self.pending_http_from_dns));
        completions.extend(self.expire_resolves()?);
        completions.extend(self.dispatch_waiting());
        self.resume_streams(&mut completions)?;
        let poll_deadline = if completions.is_empty() {
            deadline
        } else {
            Instant::now()
        };
        let events = self
            .reactor
            .poll(poll_deadline)
            .map_err(native_internal_error)?;
        completions.extend(self.process_events(events)?);
        self.resume_standalone_tcp()?;
        self.drain_dns()?;
        completions.extend(std::mem::take(&mut self.pending_http_from_dns));
        completions.extend(self.expire_resolves()?);
        completions.extend(self.dispatch_waiting());
        Ok(completions)
    }

    fn shutdown(&mut self) -> Result<(), ShutdownError> {
        let closing = self.connection_count;
        self.request_to_slot.clear();
        self.transfers.clear();
        self.request_to_resolve.clear();
        self.resolves.clear();
        #[cfg(feature = "resolver")]
        {
            self.request_to_public.clear();
            self.public_lookups.clear();
            self.pending_public.clear();
        }
        self.standalone_request_to_resolve.clear();
        self.standalone_resolves.clear();
        self.pending_http_from_dns.clear();
        self.idle.clear();
        self.idle_slots.clear();
        self.idle_count = 0;
        self.connection_count = 0;
        self.connections_per_key.clear();
        self.waiting.clear();
        self.standalone_request_to_slot.clear();
        self.standalone_pending.clear();
        self.standalone_live.clear();
        if let Some(metrics) = &self.metrics {
            for _ in 0..closing {
                metrics.connection_closed(0);
            }
            metrics.set_idle_connections(0);
            metrics.set_connection_waiters(0);
        }
        if let Some(resolver) = &mut self.resolver {
            resolver.shutdown().map_err(ShutdownError::new)?;
        }
        self.reactor.shutdown();
        Ok(())
    }

    fn poll_mode(&self) -> PollMode {
        PollMode::Interruptible {
            max_wait: NATIVE_SAFETY_POLL,
        }
    }

    fn wants_poll_without_requests(&self) -> bool {
        let has_public_lookups = {
            #[cfg(feature = "resolver")]
            {
                !self.public_lookups.is_empty()
            }
            #[cfg(not(feature = "resolver"))]
            {
                false
            }
        };
        self.idle_count != 0
            || has_public_lookups
            || !self.standalone_resolves.is_empty()
            || !self.standalone_pending.is_empty()
            || !self.standalone_live.is_empty()
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_native_resolver(&self) -> bool {
        self.resolver.is_some()
    }

    fn supports_standalone_tcp(&self) -> bool {
        true
    }

    fn submit_tcp_connect(
        &mut self,
        request: TcpConnectRequest,
        sink: TcpConnectSink,
        accepted_at: Instant,
    ) {
        let deadline = request
            .connect_timeout()
            .and_then(|timeout| accepted_at.checked_add(timeout));
        match request.target() {
            TcpConnectTarget::Literal(address) => {
                self.start_standalone_attempt(StandalonePending {
                    sink,
                    remaining: VecDeque::from([*address]),
                    connect_deadline: deadline,
                });
            }
            TcpConnectTarget::Hostname { name, port } => {
                if self.resolver.is_none() {
                    sink.fail(Error::new(
                        ErrorKind::Unsupported,
                        "hostname TCP connections require the native resolver owner",
                    ));
                    return;
                }
                if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                    sink.fail(standalone_connect_timeout());
                    return;
                }
                let key = match self.next_resolve_key() {
                    Ok(key) => key,
                    Err(error) => {
                        sink.fail(error);
                        return;
                    }
                };
                if let Err(error) = self
                    .resolver
                    .as_ref()
                    .expect("resolver presence checked")
                    .public_resolve(
                        key,
                        PublicResolveSpec {
                            host: name.clone(),
                            family: AddressFamily::Both,
                            order: AddressOrder::Ipv4ThenIpv6,
                            cache_mode: CacheMode::Use,
                            max_results: sink.max_resolve_results(),
                            expand_search: false,
                            unavailable_is_unsupported: false,
                        },
                    )
                {
                    sink.fail(error);
                    return;
                }
                self.standalone_request_to_resolve.insert(sink.id(), key);
                self.standalone_resolves.insert(
                    key,
                    StandaloneResolve {
                        sink,
                        port: *port,
                        connect_deadline: deadline,
                    },
                );
            }
        }
    }

    #[cfg(feature = "resolver")]
    fn submit_resolve(
        &mut self,
        id: RequestId,
        request: ResolveRequest,
        accepted_at: Instant,
        max_results: usize,
    ) -> Option<ResolveCompletion> {
        if self.resolver.is_none() {
            return Some(ResolveCompletion::Failed(Error::new(
                ErrorKind::Unsupported,
                "public hostname resolution is not available on this Engine",
            )));
        }
        if request
            .total_timeout()
            .and_then(|timeout| accepted_at.checked_add(timeout))
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            return Some(public_total_timeout());
        }
        let key = match self.next_resolve_key() {
            Ok(key) => key,
            Err(error) => return Some(ResolveCompletion::Failed(error)),
        };
        if let Err(error) = self
            .resolver
            .as_ref()
            .expect("resolver presence checked")
            .public_resolve(
                key,
                PublicResolveSpec {
                    host: request.name().to_owned(),
                    family: request.address_family(),
                    order: request.address_order(),
                    cache_mode: request.cache_mode(),
                    max_results,
                    expand_search: request.applies_search_suffixes(),
                    unavailable_is_unsupported: true,
                },
            )
        {
            return Some(ResolveCompletion::Failed(error));
        }
        self.request_to_public.insert(id, key);
        self.public_lookups.insert(
            key,
            PublicLookup {
                request_id: id,
                total_deadline: request
                    .total_timeout()
                    .and_then(|timeout| accepted_at.checked_add(timeout)),
            },
        );
        None
    }

    #[cfg(feature = "resolver")]
    fn poll_resolves(&mut self) -> Result<Vec<BackendResolveCompletion>, Error> {
        self.drain_dns()?;
        Ok(std::mem::take(&mut self.pending_public))
    }
}

#[cfg(feature = "resolver")]
fn public_total_timeout() -> ResolveCompletion {
    ResolveCompletion::Failed(Error::timeout(
        TimeoutKind::Total,
        "the public resolution total timeout expired",
    ))
}

fn standalone_connect_timeout() -> Error {
    Error::timeout(
        TimeoutKind::Connect,
        "the standalone TCP connection-establishment timeout expired",
    )
}

fn native_internal_error(failure: NativeFailure) -> Error {
    Error::new(ErrorKind::Internal, failure.message)
}

fn native_timeout_message(kind: TimeoutKind) -> &'static str {
    match kind {
        TimeoutKind::Connect => "the native HTTP connection-establishment timeout expired",
        TimeoutKind::Inactivity => "the native HTTP inactivity timeout expired",
        TimeoutKind::Total => "the native HTTP total request timeout expired",
        TimeoutKind::Unknown => "a native HTTP deadline expired without a matching timeout stage",
    }
}

fn native_transport_error(failure: NativeFailure) -> Error {
    match failure.kind {
        NativeFailureKind::Connect => Error::transport(TransportStage::Connect, failure.message),
        NativeFailureKind::Read => Error::transport(TransportStage::Receive, failure.message),
        NativeFailureKind::Write => Error::transport(TransportStage::Send, failure.message),
        NativeFailureKind::OutboundQueueFull => Error::limit(
            LimitKind::RequestBodyBytes,
            "the serialized native request exceeded its bounded send queue",
        ),
        NativeFailureKind::ReceiveLimit => Error::limit(
            LimitKind::ResponseBodyBytes,
            "the native response exceeded its bounded wire allowance",
        ),
        NativeFailureKind::Internal => Error::new(ErrorKind::Internal, failure.message),
    }
}

#[cfg(test)]
mod tests;
