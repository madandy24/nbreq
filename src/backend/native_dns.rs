#![cfg_attr(not(test), allow(dead_code))]

//! Private Engine-owned nonblocking DNS service.
//!
//! The sibling `native_dns_wire` module owns bounded DNS question encoding and response decoding.
//! This module owns the socket, poll loop, retry clock, resolver policy, command/result queues,
//! cancellation, wakeup, and joined shutdown.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(test)]
use super::native_dns_wire::test_support::{
    Message, MessageType, Name, Query, RData, RecordType, ResponseCode,
};
use mio::net::{TcpStream, UdpSocket};
use mio::{Interest, Token};

use super::native::NATIVE_SAFETY_POLL;
use super::native_dns_wire::{
    self as dns_wire, Name as DnsName, RData as WireRData, RecordType as DnsRecordType,
};
use super::native_poll::{NativePoll, NativeWaker, PollTarget};
use crate::types::{DNS_TRANSACTION_ID_SPACE, HTTP_DNS_TXID_RESERVE};
use crate::{AddressFamily, AddressOrder, CacheMode, DnsFailure, Error, ErrorKind, ResolveStatus};

const WAKE_TOKEN: Token = Token(0);
const SOCKET_TOKEN: Token = Token(1);
const FIRST_TCP_TOKEN: usize = 2;
const DNS_PACKET_LIMIT: usize = 4096;
const DEFAULT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_ATTEMPTS: u8 = 3;
const MAX_CNAME_HOPS: u8 = 8;
const DNS_CACHE_CAPACITY: usize = 256;
const MAX_POSITIVE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const SYSTEM_CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct ResolveKey(pub(super) u64);

pub(super) struct ResolverConfig {
    pub(super) nameservers: Vec<SocketAddr>,
    pub(super) search_suffixes: Vec<String>,
    pub(super) attempt_timeout: Duration,
    pub(super) attempts: u8,
    source: ResolverConfigSource,
}

enum ResolverConfigSource {
    Static,
    System,
    #[cfg(test)]
    Test {
        receiver: Receiver<Result<ResolverSnapshot, Error>>,
        refresh_interval: Option<Duration>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResolverSnapshot {
    nameservers: Vec<SocketAddr>,
    search_suffixes: Vec<String>,
    attempt_timeout: Duration,
    attempts: u8,
}

impl ResolverConfig {
    pub(super) fn injected(nameserver: SocketAddr) -> Self {
        Self {
            nameservers: vec![nameserver],
            search_suffixes: Vec::new(),
            attempt_timeout: DEFAULT_ATTEMPT_TIMEOUT,
            attempts: DEFAULT_ATTEMPTS,
            source: ResolverConfigSource::Static,
        }
    }

    pub(super) fn with_search_suffixes(
        mut self,
        suffixes: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        self.search_suffixes = super::native_dns_config::normalize_search_suffixes(suffixes);
        self
    }

    pub(super) fn system() -> Result<Self, Error> {
        let discovered = super::native_dns_config::discover()?;
        Ok(Self {
            nameservers: discovered.nameservers,
            search_suffixes: discovered.search_suffixes,
            attempt_timeout: discovered.attempt_timeout,
            attempts: discovered.attempts,
            source: ResolverConfigSource::System,
        })
    }

    fn snapshot(&self) -> ResolverSnapshot {
        ResolverSnapshot {
            nameservers: self.nameservers.clone(),
            search_suffixes: self.search_suffixes.clone(),
            attempt_timeout: self.attempt_timeout,
            attempts: self.attempts,
        }
    }

    fn refresh_interval(&self) -> Option<Duration> {
        match self.source {
            ResolverConfigSource::System => Some(SYSTEM_CONFIG_REFRESH_INTERVAL),
            ResolverConfigSource::Static => None,
            #[cfg(test)]
            ResolverConfigSource::Test {
                refresh_interval, ..
            } => refresh_interval,
        }
    }

    fn is_system(&self) -> bool {
        matches!(self.source, ResolverConfigSource::System)
    }

    fn rediscover(&self) -> Option<Result<ResolverSnapshot, Error>> {
        match &self.source {
            ResolverConfigSource::Static => None,
            ResolverConfigSource::System => Some(super::native_dns_config::discover().map(
                |discovered| ResolverSnapshot {
                    nameservers: discovered.nameservers,
                    search_suffixes: discovered.search_suffixes,
                    attempt_timeout: discovered.attempt_timeout,
                    attempts: discovered.attempts,
                },
            )),
            #[cfg(test)]
            ResolverConfigSource::Test { receiver, .. } => Some(match receiver.try_recv() {
                Ok(snapshot) => snapshot,
                Err(TryRecvError::Empty) => Ok(self.snapshot()),
                Err(TryRecvError::Disconnected) => Err(Error::new(
                    ErrorKind::Internal,
                    "the injected DNS configuration source disconnected",
                )),
            }),
        }
    }

    fn apply(&mut self, snapshot: ResolverSnapshot) {
        self.nameservers = snapshot.nameservers;
        self.search_suffixes = snapshot.search_suffixes;
        self.attempt_timeout = snapshot.attempt_timeout;
        self.attempts = snapshot.attempts;
    }

    #[cfg(test)]
    fn with_test_refresh_source(self) -> (Self, Sender<Result<ResolverSnapshot, Error>>) {
        self.with_test_refresh_source_and_interval(None)
    }

    #[cfg(test)]
    pub(super) fn with_test_refresh_source_and_interval(
        mut self,
        refresh_interval: Option<Duration>,
    ) -> (Self, Sender<Result<ResolverSnapshot, Error>>) {
        let (sender, receiver) = mpsc::channel();
        self.source = ResolverConfigSource::Test {
            receiver,
            refresh_interval,
        };
        (self, sender)
    }

    #[cfg(test)]
    pub(super) fn snapshot_for_test(nameserver: SocketAddr) -> ResolverSnapshot {
        Self::for_test(nameserver).snapshot()
    }

    #[cfg(test)]
    fn for_test(nameserver: SocketAddr) -> Self {
        let mut config = Self::injected(nameserver);
        config.attempt_timeout = Duration::from_millis(50);
        config.attempts = 2;
        config
    }

    #[cfg(test)]
    fn multiple_for_test(nameservers: Vec<SocketAddr>) -> Self {
        Self {
            nameservers,
            search_suffixes: Vec::new(),
            // This fixture must prove rotation, not make OS scheduling part of DNS policy. Leave
            // enough room for a loaded CI host to schedule the answering server after failover.
            attempt_timeout: Duration::from_secs(1),
            attempts: 1,
            source: ResolverConfigSource::Static,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResolveAnswer {
    pub(super) addresses: Vec<IpAddr>,
    pub(super) ttl: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResolveFailure {
    pub(super) message: String,
    class: DnsFailure,
}

impl ResolveFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            class: DnsFailure::Unknown,
        }
    }

    fn classified(class: DnsFailure, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            class,
        }
    }
}

#[derive(Debug)]
pub(super) enum PublicLookupOutcome {
    Completed {
        #[cfg(feature = "resolver")]
        name: String,
        status: ResolveStatus,
        addresses: Vec<IpAddr>,
        #[cfg(feature = "resolver")]
        valid_until: Option<Instant>,
        #[cfg(feature = "resolver")]
        from_cache: bool,
        #[cfg(feature = "resolver")]
        candidate_name: Option<String>,
    },
    Failed(Error),
}

#[derive(Debug)]
pub(super) struct ResolveResult {
    pub(super) key: ResolveKey,
    pub(super) result: Result<ResolveAnswer, ResolveFailure>,
    pub(super) public: Option<PublicLookupOutcome>,
}

impl ResolveResult {
    fn http(key: ResolveKey, result: Result<ResolveAnswer, ResolveFailure>) -> Self {
        Self {
            key,
            result,
            public: None,
        }
    }

    fn public(key: ResolveKey, public: PublicLookupOutcome) -> Self {
        Self {
            key,
            result: Err(ResolveFailure::new("public lookup")),
            public: Some(public),
        }
    }
}

pub(super) struct PublicResolveSpec {
    pub(super) host: String,
    pub(super) family: AddressFamily,
    pub(super) order: AddressOrder,
    pub(super) cache_mode: CacheMode,
    pub(super) max_results: usize,
    pub(super) expand_search: bool,
    pub(super) unavailable_is_unsupported: bool,
}

enum Command {
    Resolve {
        key: ResolveKey,
        host: String,
    },
    PublicResolve {
        key: ResolveKey,
        spec: PublicResolveSpec,
    },
    Cancel(ResolveKey),
    #[cfg(test)]
    RefreshConfig(Sender<()>),
    Shutdown,
}

#[derive(Clone)]
enum QueryPolicy {
    Http(RetryPolicy),
    Public(Box<PublicSession>),
}

#[derive(Clone, Copy)]
struct RetryPolicy {
    attempt_limit: u8,
    attempt_timeout: Duration,
}

#[derive(Clone)]
struct PublicSession {
    #[cfg(feature = "resolver")]
    identity: String,
    /// Queried candidate for the current/winning lookup. Exact lookups keep this equal to
    /// `identity`. Search expansion stores the suffix-expanded question name.
    candidate: String,
    candidates: Vec<String>,
    candidate_index: usize,
    family: AddressFamily,
    order: AddressOrder,
    cache_mode: CacheMode,
    max_results: usize,
    ipv4: Option<FamilyOutcome>,
    ipv6: Option<FamilyOutcome>,
    /// True only while every candidate so far required no network query.
    from_cache: bool,
    saw_nodata: bool,
    /// `None` until a negative candidate is recorded; `Some(None)` if any contributing negative
    /// lacks cache validity; otherwise the earliest contributing expiry.
    negative_validity: Option<Option<Instant>>,
    attempt_limit: u8,
    attempt_timeout: Duration,
}

#[derive(Clone)]
struct FamilyOutcome {
    status: ResolveStatus,
    addresses: Vec<IpAddr>,
    valid_until: Option<Instant>,
}

struct PendingQuery {
    key: ResolveKey,
    host: DnsName,
    record_type: DnsRecordType,
    cname_hops: u8,
    wire: Vec<u8>,
    attempts_sent: u8,
    attempt_limit: u8,
    attempt_timeout: Duration,
    servers_tried: usize,
    last_udp_generation: u64,
    next_attempt: Instant,
    transport: QueryTransport,
    policy: QueryPolicy,
}

enum QueryTransport {
    Udp,
    Tcp(TcpFallback),
}

struct TcpFallback {
    stream: TcpStream,
    token: Token,
    outbound: Vec<u8>,
    written: usize,
    inbound: Vec<u8>,
    expected: Option<usize>,
}

struct ResolverState {
    pending: HashMap<u16, PendingQuery>,
    by_key: HashMap<ResolveKey, u16>,
    tcp_by_token: HashMap<Token, u16>,
    next_id: u16,
    next_tcp_token: usize,
    current_nameserver: usize,
    udp_generation: u64,
    config_generation: u64,
    config_available: bool,
    config_dirty: bool,
    next_config_refresh: Option<Instant>,
    cache: DnsCache,
    before_first_poll: Option<Sender<()>>,
}

#[derive(Clone)]
struct CacheEntry {
    result: Result<ResolveAnswer, ResolveFailure>,
    expires: Instant,
    last_used: u64,
}

#[derive(Clone)]
struct FamilyCacheEntry {
    record: CachedFamily,
    expires: Instant,
    last_used: u64,
}

#[derive(Clone)]
enum CachedFamily {
    Answer(Vec<IpAddr>),
    NameNotFound,
    NoData,
}

struct DnsCache {
    entries: HashMap<DnsName, CacheEntry>,
    family_entries: HashMap<(DnsName, DnsRecordType), FamilyCacheEntry>,
    next_use: u64,
}

impl DnsCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            family_entries: HashMap::new(),
            next_use: 1,
        }
    }

    fn get(
        &mut self,
        name: &DnsName,
        now: Instant,
    ) -> Option<Result<ResolveAnswer, ResolveFailure>> {
        if self.entries.get(name)?.expires <= now {
            self.entries.remove(name);
            return None;
        }
        let last_used = self.next_use;
        self.bump_use();
        let entry = self.entries.get_mut(name)?;
        entry.last_used = last_used;
        Some(entry.result.clone())
    }

    fn insert(
        &mut self,
        name: DnsName,
        result: Result<ResolveAnswer, ResolveFailure>,
        ttl: Duration,
        maximum_ttl: Duration,
        now: Instant,
    ) {
        if ttl.is_zero() || DNS_CACHE_CAPACITY == 0 {
            return;
        }
        self.entries.retain(|_, entry| entry.expires > now);
        if self.entries.len() >= DNS_CACHE_CAPACITY && !self.entries.contains_key(&name) {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(name, _)| name.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        let ttl = ttl.min(maximum_ttl);
        let Some(expires) = now.checked_add(ttl) else {
            return;
        };
        let last_used = self.next_use;
        self.bump_use();
        self.entries.insert(
            name,
            CacheEntry {
                result,
                expires,
                last_used,
            },
        );
    }

    fn get_family(
        &mut self,
        name: &DnsName,
        record_type: DnsRecordType,
        now: Instant,
    ) -> Option<(CachedFamily, Instant)> {
        let key = (name.clone(), record_type);
        if self.family_entries.get(&key)?.expires <= now {
            self.family_entries.remove(&key);
            return None;
        }
        let last_used = self.next_use;
        self.bump_use();
        let entry = self.family_entries.get_mut(&key)?;
        entry.last_used = last_used;
        Some((entry.record.clone(), entry.expires))
    }

    fn insert_family(
        &mut self,
        name: DnsName,
        record_type: DnsRecordType,
        record: CachedFamily,
        ttl: Duration,
        maximum_ttl: Duration,
        now: Instant,
    ) -> Option<Instant> {
        if ttl.is_zero() || DNS_CACHE_CAPACITY == 0 {
            return None;
        }
        self.family_entries.retain(|_, entry| entry.expires > now);
        let key = (name, record_type);
        if self.family_entries.len() >= DNS_CACHE_CAPACITY
            && !self.family_entries.contains_key(&key)
        {
            if let Some(oldest) = self
                .family_entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(name, _)| name.clone())
            {
                self.family_entries.remove(&oldest);
            }
        }
        let ttl = ttl.min(maximum_ttl);
        let expires = now.checked_add(ttl)?;
        let last_used = self.next_use;
        self.bump_use();
        self.family_entries.insert(
            key,
            FamilyCacheEntry {
                record,
                expires,
                last_used,
            },
        );
        Some(expires)
    }

    fn remove_http(&mut self, name: &DnsName) {
        self.entries.remove(name);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.family_entries.clear();
    }

    fn bump_use(&mut self) {
        self.next_use = self.next_use.checked_add(1).unwrap_or_else(|| {
            self.entries.clear();
            self.family_entries.clear();
            1
        });
    }
}

pub(super) struct NativeResolver {
    commands: Sender<Command>,
    results: Receiver<ResolveResult>,
    controls: Receiver<ResolverControl>,
    waker: NativeWaker,
    joined: Option<JoinHandle<()>>,
}

enum ResolverControl {
    EvictIdleHttp(u64),
}

struct ResolverChannels {
    commands: Receiver<Command>,
    results: Sender<ResolveResult>,
    controls: Sender<ResolverControl>,
    result_waker: NativeWaker,
}

#[cfg(target_os = "macos")]
struct ResolverConfigMonitor(Option<nbreq_darwin::ResolverChangeMonitor>);

#[cfg(target_os = "macos")]
impl ResolverConfigMonitor {
    fn new(enabled: bool) -> Result<Self, Error> {
        if !enabled {
            return Ok(Self(None));
        }
        nbreq_darwin::ResolverChangeMonitor::new()
            .map(|monitor| Self(Some(monitor)))
            .map_err(|error| {
                Error::new(
                    ErrorKind::Unsupported,
                    format!("macOS DNS notification setup failed: {error}"),
                )
            })
    }

    fn pump(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(nbreq_darwin::ResolverChangeMonitor::pump)
    }
}

#[cfg(not(target_os = "macos"))]
struct ResolverConfigMonitor;

#[cfg(not(target_os = "macos"))]
impl ResolverConfigMonitor {
    fn new(_enabled: bool) -> Result<Self, Error> {
        Ok(Self)
    }

    fn pump(&self) -> bool {
        false
    }
}

impl NativeResolver {
    pub(super) fn new(config: ResolverConfig, result_waker: NativeWaker) -> Result<Self, Error> {
        Self::new_inner(config, result_waker, None)
    }

    fn new_inner(
        config: ResolverConfig,
        result_waker: NativeWaker,
        before_first_poll: Option<Sender<()>>,
    ) -> Result<Self, Error> {
        if config.attempts == 0 || config.attempt_timeout.is_zero() {
            return Err(Error::new(
                ErrorKind::Internal,
                "native resolver retry configuration must be nonzero",
            ));
        }
        let (mut poll, waker) = NativePoll::new(16, WAKE_TOKEN)
            .map_err(|error| resolver_internal("poll creation", &error))?;
        let (mut socket, current_nameserver) = connect_nameserver(&config.nameservers)?;
        poll.register(&mut socket, SOCKET_TOKEN, Interest::READABLE)
            .map_err(|error| resolver_internal("socket registration", &error))?;
        let mut initial_id = [0_u8; 2];
        getrandom::fill(&mut initial_id).map_err(|error| {
            Error::new(
                ErrorKind::Internal,
                format!("native resolver transaction randomization failed: {error}"),
            )
        })?;

        let (command_tx, command_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let (control_tx, control_rx) = mpsc::channel();
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let next_config_refresh = config
            .refresh_interval()
            .and_then(|interval| Instant::now().checked_add(interval));
        let state = ResolverState {
            pending: HashMap::new(),
            by_key: HashMap::new(),
            tcp_by_token: HashMap::new(),
            next_id: u16::from_ne_bytes(initial_id),
            next_tcp_token: FIRST_TCP_TOKEN,
            current_nameserver,
            udp_generation: 1,
            config_generation: 1,
            config_available: true,
            config_dirty: false,
            next_config_refresh,
            cache: DnsCache::new(),
            before_first_poll,
        };
        let joined = thread::Builder::new()
            .name("nbreq-native-dns".to_owned())
            .spawn(move || {
                let monitor = match ResolverConfigMonitor::new(config.is_system()) {
                    Ok(monitor) => {
                        let _startup_result = startup_tx.send(Ok(()));
                        monitor
                    }
                    Err(error) => {
                        let _startup_result = startup_tx.send(Err(error));
                        return;
                    }
                };
                resolver_main(
                    &mut poll,
                    Some(socket),
                    ResolverChannels {
                        commands: command_rx,
                        results: result_tx,
                        controls: control_tx,
                        result_waker,
                    },
                    config,
                    state,
                    monitor,
                );
            })
            .map_err(|error| resolver_internal("thread start", &error))?;
        match startup_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _join_result = joined.join();
                return Err(error);
            }
            Err(_) => {
                let _join_result = joined.join();
                return Err(Error::new(
                    ErrorKind::Internal,
                    "native resolver startup channel disconnected",
                ));
            }
        }
        Ok(Self {
            commands: command_tx,
            results: result_rx,
            controls: control_rx,
            waker,
            joined: Some(joined),
        })
    }

    pub(super) fn resolve(&self, key: ResolveKey, host: String) -> Result<(), Error> {
        self.send(Command::Resolve { key, host })
    }

    pub(super) fn public_resolve(
        &self,
        key: ResolveKey,
        spec: PublicResolveSpec,
    ) -> Result<(), Error> {
        self.send(Command::PublicResolve { key, spec })
    }

    pub(super) fn cancel(&self, key: ResolveKey) -> Result<(), Error> {
        self.send(Command::Cancel(key))
    }

    #[cfg(test)]
    pub(super) fn refresh_config_for_test(&self) -> Result<(), Error> {
        let (acknowledge, acknowledged) = mpsc::channel();
        self.send(Command::RefreshConfig(acknowledge))?;
        acknowledged
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| Error::new(ErrorKind::Internal, "test DNS refresh was not acknowledged"))
    }

    pub(super) fn drain(&self) -> Result<Vec<ResolveResult>, Error> {
        let mut results = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(result) => results.push(result),
                Err(TryRecvError::Empty) => return Ok(results),
                Err(TryRecvError::Disconnected) => {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "native resolver result channel disconnected",
                    ));
                }
            }
        }
    }

    pub(super) fn take_idle_http_eviction(&self) -> Result<Option<u64>, Error> {
        let mut generation = None;
        loop {
            match self.controls.try_recv() {
                Ok(ResolverControl::EvictIdleHttp(current)) => generation = Some(current),
                Err(TryRecvError::Empty) => return Ok(generation),
                Err(TryRecvError::Disconnected) => {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "native resolver control channel disconnected",
                    ));
                }
            }
        }
    }

    pub(super) fn shutdown(&mut self) -> Result<(), Error> {
        let send_result = self.send(Command::Shutdown);
        let join_result = self.joined.take().map_or(Ok(()), |joined| {
            joined
                .join()
                .map_err(|_| Error::new(ErrorKind::Internal, "native resolver thread panicked"))
        });
        send_result.and(join_result)
    }

    fn send(&self, command: Command) -> Result<(), Error> {
        self.commands.send(command).map_err(|_| {
            Error::new(
                ErrorKind::Internal,
                "native resolver command channel disconnected",
            )
        })?;
        self.waker
            .wake()
            .map_err(|error| resolver_internal("command wake", &error))
    }
}

fn connect_nameserver(nameservers: &[SocketAddr]) -> Result<(UdpSocket, usize), Error> {
    let mut last_error = None;
    for (index, nameserver) in nameservers.iter().enumerate() {
        match connect_one_nameserver(*nameserver) {
            Ok(socket) => return Ok((socket, index)),
            Err(error) => last_error = Some(error),
        }
    }
    let detail = last_error.map_or_else(
        || "no DNS nameservers were configured".to_owned(),
        |error| format!("no configured DNS nameserver was reachable: {error}"),
    );
    Err(Error::new(ErrorKind::Internal, detail))
}

fn connect_one_nameserver(nameserver: SocketAddr) -> io::Result<UdpSocket> {
    let bind_address = match nameserver {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = UdpSocket::bind(bind_address)?;
    socket.connect(nameserver)?;
    Ok(socket)
}

impl Drop for NativeResolver {
    fn drop(&mut self) {
        let _shutdown_result = self.shutdown();
    }
}

fn resolver_main(
    poll: &mut NativePoll,
    mut socket: Option<UdpSocket>,
    channels: ResolverChannels,
    mut config: ResolverConfig,
    mut state: ResolverState,
    monitor: ResolverConfigMonitor,
) {
    let ResolverChannels {
        commands,
        results,
        controls,
        result_waker,
    } = channels;
    loop {
        if monitor.pump() {
            state.config_dirty = true;
        }
        if state
            .next_config_refresh
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            state.config_dirty = true;
        }
        refresh_configuration_if_dirty(
            &mut socket,
            poll,
            &mut config,
            &mut state,
            &results,
            &controls,
            &result_waker,
        );
        let mut stop = false;
        #[cfg(test)]
        let mut refresh_acks = Vec::new();
        loop {
            match commands.try_recv() {
                Ok(Command::Resolve { key, host }) => {
                    if let Some(previous) = state.by_key.remove(&key) {
                        remove_query(previous, &mut state, poll);
                    }
                    if !state.config_available {
                        send_result(
                            &results,
                            &result_waker,
                            ResolveResult::http(key, Err(configuration_unavailable_failure())),
                        );
                        continue;
                    }
                    let name = match parse_dns_name(&host) {
                        Ok(name) => name,
                        Err(failure) => {
                            send_result(
                                &results,
                                &result_waker,
                                ResolveResult::http(key, Err(failure)),
                            );
                            continue;
                        }
                    };
                    if let Some(result) = state.cache.get(&name, Instant::now()) {
                        send_result(&results, &result_waker, ResolveResult::http(key, result));
                        continue;
                    }
                    match prepare_name_query(
                        key,
                        name,
                        DnsRecordType::A,
                        0,
                        &state.pending,
                        &mut state.next_id,
                        QueryPolicy::Http(RetryPolicy {
                            attempt_limit: config.attempts,
                            attempt_timeout: config.attempt_timeout,
                        }),
                    ) {
                        Ok((id, query)) => {
                            state.by_key.insert(key, id);
                            state.pending.insert(id, query);
                        }
                        Err(failure) => send_result(
                            &results,
                            &result_waker,
                            ResolveResult::http(key, Err(failure)),
                        ),
                    }
                }
                Ok(Command::PublicResolve { key, spec }) => {
                    if let Some(previous) = state.by_key.remove(&key) {
                        remove_query(previous, &mut state, poll);
                    }
                    if !state.config_available {
                        let error = if spec.unavailable_is_unsupported {
                            Error::new(
                                ErrorKind::Unsupported,
                                "the current system DNS configuration is unavailable or unsupported",
                            )
                        } else {
                            public_error(configuration_unavailable_failure())
                        };
                        send_result(
                            &results,
                            &result_waker,
                            ResolveResult::public(key, PublicLookupOutcome::Failed(error)),
                        );
                        continue;
                    }
                    begin_public_resolve(
                        key,
                        spec,
                        &config.search_suffixes,
                        RetryPolicy {
                            attempt_limit: config.attempts,
                            attempt_timeout: config.attempt_timeout,
                        },
                        &mut state,
                        &results,
                        &result_waker,
                    );
                }
                Ok(Command::Cancel(key)) => {
                    if let Some(id) = state.by_key.remove(&key) {
                        remove_query(id, &mut state, poll);
                    }
                }
                #[cfg(test)]
                Ok(Command::RefreshConfig(acknowledge)) => {
                    state.config_dirty = true;
                    refresh_acks.push(acknowledge);
                }
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => {
                    stop = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if stop {
            return;
        }

        refresh_configuration_if_dirty(
            &mut socket,
            poll,
            &mut config,
            &mut state,
            &results,
            &controls,
            &result_waker,
        );
        #[cfg(test)]
        for acknowledge in refresh_acks {
            let _acknowledge_result = acknowledge.send(());
        }

        if let Some(active_socket) = socket.as_mut() {
            if transmit_due(
                active_socket,
                &mut state,
                &results,
                &result_waker,
                &config,
                poll,
            ) {
                state.config_dirty = true;
            }
        }
        refresh_configuration_if_dirty(
            &mut socket,
            poll,
            &mut config,
            &mut state,
            &results,
            &controls,
            &result_waker,
        );
        let now = Instant::now();
        let next_query = state
            .pending
            .values()
            .map(|query| query.next_attempt.saturating_duration_since(now))
            .min();
        let next_refresh = state
            .next_config_refresh
            .map(|deadline| deadline.saturating_duration_since(now));
        let timeout = next_query
            .into_iter()
            .chain(next_refresh)
            .min()
            .unwrap_or(NATIVE_SAFETY_POLL)
            .min(NATIVE_SAFETY_POLL);
        if let Some(barrier) = state.before_first_poll.take() {
            let _send_result = barrier.send(());
        }
        let mut targets = Vec::with_capacity(state.tcp_by_token.len() + 1);
        if let Some(active_socket) = socket.as_ref() {
            targets.push(PollTarget::new(
                SOCKET_TOKEN,
                active_socket,
                Interest::READABLE,
            ));
        }
        targets.extend(state.pending.values().filter_map(|query| {
            let QueryTransport::Tcp(tcp) = &query.transport else {
                return None;
            };
            let interest = if tcp.written < tcp.outbound.len() {
                Interest::READABLE.add(Interest::WRITABLE)
            } else {
                Interest::READABLE
            };
            Some(PollTarget::new(tcp.token, &tcp.stream, interest))
        }));
        let events = match poll.poll(&targets, timeout, WAKE_TOKEN) {
            Ok(events) => events,
            Err(_) => {
                fail_all(
                    &mut state.pending,
                    &mut state.by_key,
                    &results,
                    &result_waker,
                    "native resolver poll failed",
                );
                return;
            }
        };
        let socket_ready = events
            .iter()
            .any(|event| event.token == SOCKET_TOKEN && event.readable);
        if socket_ready {
            if let Some(active_socket) = socket.as_mut() {
                if receive_packets(
                    active_socket,
                    &mut state,
                    &results,
                    &result_waker,
                    poll,
                    &config,
                ) && replace_current_nameserver(
                    active_socket,
                    poll,
                    &config,
                    state.current_nameserver,
                ) {
                    advance_udp_generation(&mut state.udp_generation);
                }
            }
        }
        let tcp_events = events
            .iter()
            .filter(|event| event.token.0 >= FIRST_TCP_TOKEN)
            .map(|event| (event.token, event.readable, event.writable))
            .collect::<Vec<_>>();
        for (token, readable, writable) in tcp_events {
            receive_tcp(
                token,
                readable,
                writable,
                poll,
                &mut state,
                &results,
                &result_waker,
            );
        }
    }
}

fn parse_dns_name(host: &str) -> Result<DnsName, ResolveFailure> {
    DnsName::from_ascii(host).map_err(|_| {
        ResolveFailure::classified(DnsFailure::Protocol, "the DNS hostname is invalid")
    })
}

fn configuration_unavailable_failure() -> ResolveFailure {
    ResolveFailure::classified(
        DnsFailure::NoNameserver,
        "the current system DNS configuration is unavailable or unsupported",
    )
}

fn refresh_configuration_if_dirty(
    socket: &mut Option<UdpSocket>,
    poll: &mut NativePoll,
    config: &mut ResolverConfig,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    controls: &Sender<ResolverControl>,
    result_waker: &NativeWaker,
) {
    if !state.config_dirty {
        return;
    }
    state.config_dirty = false;
    state.next_config_refresh = config
        .refresh_interval()
        .and_then(|interval| Instant::now().checked_add(interval));
    let Some(discovered) = config.rediscover() else {
        return;
    };
    let snapshot = match discovered {
        Ok(snapshot)
            if !snapshot.nameservers.is_empty()
                && snapshot.attempts != 0
                && !snapshot.attempt_timeout.is_zero() =>
        {
            snapshot
        }
        Ok(_) => {
            transition_configuration_unavailable(
                socket,
                poll,
                state,
                results,
                controls,
                result_waker,
            );
            return;
        }
        Err(_) => {
            transition_configuration_unavailable(
                socket,
                poll,
                state,
                results,
                controls,
                result_waker,
            );
            return;
        }
    };

    let current = config.snapshot();
    if state.config_available && snapshot == current {
        return;
    }
    let route_changed = !state.config_available || snapshot.nameservers != current.nameservers;
    if !route_changed {
        config.apply(snapshot);
        advance_config_generation(&mut state.config_generation);
        return;
    }

    fail_pending_for_configuration_change(state, poll, results, result_waker);
    state.cache.clear();
    config.apply(snapshot);
    state.current_nameserver = 0;
    state.config_available = install_configured_socket(socket, poll, config);
    advance_config_generation(&mut state.config_generation);
    send_control(
        controls,
        result_waker,
        ResolverControl::EvictIdleHttp(state.config_generation),
    );
}

fn transition_configuration_unavailable(
    socket: &mut Option<UdpSocket>,
    poll: &mut NativePoll,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    controls: &Sender<ResolverControl>,
    result_waker: &NativeWaker,
) {
    if !state.config_available {
        return;
    }
    fail_pending_for_configuration_change(state, poll, results, result_waker);
    state.cache.clear();
    retire_configured_socket(socket, poll);
    state.config_available = false;
    advance_config_generation(&mut state.config_generation);
    send_control(
        controls,
        result_waker,
        ResolverControl::EvictIdleHttp(state.config_generation),
    );
}

fn fail_pending_for_configuration_change(
    state: &mut ResolverState,
    poll: &mut NativePoll,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    let ids = state.pending.keys().copied().collect::<Vec<_>>();
    for id in ids {
        let Some(query) = remove_query(id, state, poll) else {
            continue;
        };
        state.by_key.remove(&query.key);
        emit_failure(
            query,
            ResolveFailure::classified(
                DnsFailure::NoNameserver,
                "the system DNS configuration changed while the lookup was pending",
            ),
            results,
            result_waker,
        );
    }
}

fn install_configured_socket(
    socket: &mut Option<UdpSocket>,
    poll: &mut NativePoll,
    config: &ResolverConfig,
) -> bool {
    let Some(nameserver) = config.nameservers.first().copied() else {
        retire_configured_socket(socket, poll);
        return false;
    };
    let Ok(mut replacement) = connect_one_nameserver(nameserver) else {
        retire_configured_socket(socket, poll);
        return false;
    };
    if poll
        .register(&mut replacement, SOCKET_TOKEN, Interest::READABLE)
        .is_err()
    {
        retire_configured_socket(socket, poll);
        return false;
    }
    retire_configured_socket(socket, poll);
    *socket = Some(replacement);
    true
}

fn retire_configured_socket(socket: &mut Option<UdpSocket>, poll: &mut NativePoll) {
    if let Some(mut socket) = socket.take() {
        let _deregister_result = poll.deregister(&mut socket);
    }
}

fn advance_config_generation(generation: &mut u64) {
    *generation = generation.wrapping_add(1);
    if *generation == 0 {
        *generation = 1;
    }
}

fn send_control(controls: &Sender<ResolverControl>, waker: &NativeWaker, control: ResolverControl) {
    if controls.send(control).is_ok() {
        let _wake_result = waker.wake();
    }
}

fn public_error(failure: ResolveFailure) -> Error {
    Error::dns(failure.class, failure.message)
}

fn emit_failure(
    query: PendingQuery,
    failure: ResolveFailure,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    match query.policy {
        QueryPolicy::Http(_) => send_result(
            results,
            result_waker,
            ResolveResult::http(query.key, Err(failure)),
        ),
        QueryPolicy::Public(_) => send_result(
            results,
            result_waker,
            ResolveResult::public(
                query.key,
                PublicLookupOutcome::Failed(public_error(failure)),
            ),
        ),
    }
}

fn cached_family_outcome(record: CachedFamily, expires: Instant) -> FamilyOutcome {
    match record {
        CachedFamily::Answer(addresses) => FamilyOutcome {
            status: ResolveStatus::Answer,
            addresses,
            valid_until: Some(expires),
        },
        CachedFamily::NameNotFound => FamilyOutcome {
            status: ResolveStatus::NameNotFound,
            addresses: Vec::new(),
            valid_until: Some(expires),
        },
        CachedFamily::NoData => FamilyOutcome {
            status: ResolveStatus::NoData,
            addresses: Vec::new(),
            valid_until: Some(expires),
        },
    }
}

fn session_needs(session: &PublicSession) -> Option<DnsRecordType> {
    match session.family {
        AddressFamily::Ipv4 if session.ipv4.is_none() => Some(DnsRecordType::A),
        AddressFamily::Ipv6 if session.ipv6.is_none() => Some(DnsRecordType::AAAA),
        AddressFamily::Both if session.ipv4.is_none() => Some(DnsRecordType::A),
        AddressFamily::Both if session.ipv6.is_none() => Some(DnsRecordType::AAAA),
        _ => None,
    }
}

fn begin_public_resolve(
    key: ResolveKey,
    spec: PublicResolveSpec,
    search_suffixes: &[String],
    retry: RetryPolicy,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    let mut session = PublicSession {
        #[cfg(feature = "resolver")]
        identity: spec.host.clone(),
        candidate: String::new(),
        candidates: public_search_candidates(&spec.host, spec.expand_search, search_suffixes),
        candidate_index: 0,
        family: spec.family,
        order: spec.order,
        cache_mode: spec.cache_mode,
        max_results: spec.max_results,
        ipv4: None,
        ipv6: None,
        from_cache: spec.cache_mode == CacheMode::Use,
        saw_nodata: false,
        negative_validity: None,
        attempt_limit: retry.attempt_limit,
        attempt_timeout: retry.attempt_timeout,
    };
    let Some(first) = session.candidates.first().cloned() else {
        send_result(
            results,
            result_waker,
            ResolveResult::public(
                key,
                PublicLookupOutcome::Failed(public_error(ResolveFailure::classified(
                    DnsFailure::Protocol,
                    "the public resolver produced no DNS search candidates",
                ))),
            ),
        );
        return;
    };
    session.candidate = first;
    start_current_candidate(key, session, state, results, result_waker);
}

fn public_search_candidates(
    identity: &str,
    expand_search: bool,
    suffixes: &[String],
) -> Vec<String> {
    if !expand_search {
        return vec![identity.to_owned()];
    }
    let dotted = identity.contains('.');
    let mut seen = HashSet::from([identity.to_owned()]);
    let mut candidates = Vec::new();
    if dotted {
        candidates.push(identity.to_owned());
    }
    for suffix in suffixes {
        let combined = format!("{identity}.{suffix}");
        let Ok(parsed) = crate::dns::normalize_dns_name(&combined) else {
            continue;
        };
        if !seen.insert(parsed.identity.clone()) {
            continue;
        }
        candidates.push(parsed.identity);
    }
    if candidates.is_empty() {
        candidates.push(identity.to_owned());
    }
    candidates
}

fn start_current_candidate(
    key: ResolveKey,
    mut session: PublicSession,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    session.ipv4 = None;
    session.ipv6 = None;
    let family = session.family;
    let cache_mode = session.cache_mode;
    let name = match parse_dns_name(&session.candidate) {
        Ok(name) => name,
        Err(failure) => {
            send_result(
                results,
                result_waker,
                ResolveResult::public(key, PublicLookupOutcome::Failed(public_error(failure))),
            );
            return;
        }
    };
    if cache_mode == CacheMode::Use {
        let now = Instant::now();
        if matches!(family, AddressFamily::Ipv4 | AddressFamily::Both) {
            if let Some((record, expires)) = state.cache.get_family(&name, DnsRecordType::A, now) {
                session.ipv4 = Some(cached_family_outcome(record, expires));
            }
        }
        if matches!(family, AddressFamily::Ipv6 | AddressFamily::Both) {
            if let Some((record, expires)) = state.cache.get_family(&name, DnsRecordType::AAAA, now)
            {
                session.ipv6 = Some(cached_family_outcome(record, expires));
            }
        }
        if family == AddressFamily::Both {
            if session
                .ipv4
                .as_ref()
                .is_some_and(|outcome| outcome.status == ResolveStatus::NameNotFound)
            {
                session.ipv6 = session.ipv4.clone();
            } else if session
                .ipv6
                .as_ref()
                .is_some_and(|outcome| outcome.status == ResolveStatus::NameNotFound)
            {
                session.ipv4 = session.ipv6.clone();
            }
        }
        if session_needs(&session).is_none() {
            after_candidate_families_complete(
                key,
                None,
                session,
                &name,
                state,
                results,
                result_waker,
            );
            return;
        }
        session.from_cache = false;
    }
    let Some(record_type) = session_needs(&session) else {
        after_candidate_families_complete(key, None, session, &name, state, results, result_waker);
        return;
    };
    match prepare_name_query(
        key,
        name,
        record_type,
        0,
        &state.pending,
        &mut state.next_id,
        QueryPolicy::Public(Box::new(session)),
    ) {
        Ok((id, query)) => {
            state.by_key.insert(key, id);
            state.pending.insert(id, query);
        }
        Err(failure) => send_result(
            results,
            result_waker,
            ResolveResult::public(key, PublicLookupOutcome::Failed(public_error(failure))),
        ),
    }
}

fn complete_public_session(session: &PublicSession) -> PublicLookupOutcome {
    let (status, mut addresses, family_valid_until) = match session.family {
        AddressFamily::Ipv4 => family_response(session.ipv4.as_ref()),
        AddressFamily::Ipv6 => family_response(session.ipv6.as_ref()),
        AddressFamily::Both => combine_both(session),
    };
    if addresses.len() > session.max_results {
        addresses.truncate(session.max_results);
    }
    #[cfg(feature = "resolver")]
    let (status, valid_until, candidate_name) = if status == ResolveStatus::Answer {
        (status, family_valid_until, Some(session.candidate.clone()))
    } else {
        (
            if session.saw_nodata {
                ResolveStatus::NoData
            } else {
                ResolveStatus::NameNotFound
            },
            session.negative_validity.flatten(),
            None,
        )
    };
    #[cfg(not(feature = "resolver"))]
    let status = if status == ResolveStatus::Answer {
        status
    } else if session.saw_nodata {
        ResolveStatus::NoData
    } else {
        ResolveStatus::NameNotFound
    };
    #[cfg(not(feature = "resolver"))]
    let _ = family_valid_until;
    PublicLookupOutcome::Completed {
        #[cfg(feature = "resolver")]
        name: session.identity.clone(),
        status,
        addresses,
        #[cfg(feature = "resolver")]
        valid_until,
        #[cfg(feature = "resolver")]
        from_cache: session.from_cache,
        #[cfg(feature = "resolver")]
        candidate_name,
    }
}

fn family_response(
    outcome: Option<&FamilyOutcome>,
) -> (ResolveStatus, Vec<IpAddr>, Option<Instant>) {
    match outcome {
        Some(outcome) => (
            outcome.status,
            outcome.addresses.clone(),
            outcome.valid_until,
        ),
        None => (ResolveStatus::NoData, Vec::new(), None),
    }
}

fn combine_both(session: &PublicSession) -> (ResolveStatus, Vec<IpAddr>, Option<Instant>) {
    let ipv4 = session.ipv4.as_ref();
    let ipv6 = session.ipv6.as_ref();
    if ipv4.is_some_and(|outcome| outcome.status == ResolveStatus::NameNotFound)
        || ipv6.is_some_and(|outcome| outcome.status == ResolveStatus::NameNotFound)
    {
        return (
            ResolveStatus::NameNotFound,
            Vec::new(),
            min_valid_until(ipv4, ipv6),
        );
    }
    let mut ipv4_addresses = ipv4
        .filter(|outcome| outcome.status == ResolveStatus::Answer)
        .map(|outcome| outcome.addresses.clone())
        .unwrap_or_default();
    let mut ipv6_addresses = ipv6
        .filter(|outcome| outcome.status == ResolveStatus::Answer)
        .map(|outcome| outcome.addresses.clone())
        .unwrap_or_default();
    if ipv4_addresses.len() + ipv6_addresses.len() > session.max_results {
        let remaining = session.max_results;
        match session.order {
            AddressOrder::Ipv4ThenIpv6 => {
                ipv4_addresses.truncate(remaining);
                ipv6_addresses.truncate(remaining.saturating_sub(ipv4_addresses.len()));
            }
            AddressOrder::Ipv6ThenIpv4 => {
                ipv6_addresses.truncate(remaining);
                ipv4_addresses.truncate(remaining.saturating_sub(ipv6_addresses.len()));
            }
        }
    }
    if ipv4_addresses.is_empty() && ipv6_addresses.is_empty() {
        return (
            ResolveStatus::NoData,
            Vec::new(),
            min_valid_until(ipv4, ipv6),
        );
    }
    let addresses = match session.order {
        AddressOrder::Ipv4ThenIpv6 => {
            let mut addresses = ipv4_addresses;
            addresses.extend(ipv6_addresses);
            addresses
        }
        AddressOrder::Ipv6ThenIpv4 => {
            let mut addresses = ipv6_addresses;
            addresses.extend(ipv4_addresses);
            addresses
        }
    };
    (
        ResolveStatus::Answer,
        addresses,
        min_valid_until(ipv4, ipv6),
    )
}

fn min_valid_until(ipv4: Option<&FamilyOutcome>, ipv6: Option<&FamilyOutcome>) -> Option<Instant> {
    match (
        ipv4.and_then(|outcome| outcome.valid_until),
        ipv6.and_then(|outcome| outcome.valid_until),
    ) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) if ipv6.is_none() => Some(left),
        (None, Some(right)) if ipv4.is_none() => Some(right),
        _ => None,
    }
}

fn prepare_name_query(
    key: ResolveKey,
    name: DnsName,
    record_type: DnsRecordType,
    cname_hops: u8,
    pending: &HashMap<u16, PendingQuery>,
    next_id: &mut u16,
    policy: QueryPolicy,
) -> Result<(u16, PendingQuery), ResolveFailure> {
    let public = matches!(policy, QueryPolicy::Public(_));
    let retry = match &policy {
        QueryPolicy::Http(retry) => *retry,
        QueryPolicy::Public(session) => RetryPolicy {
            attempt_limit: session.attempt_limit,
            attempt_timeout: session.attempt_timeout,
        },
    };
    let id = allocate_id(pending, next_id, public).ok_or_else(|| {
        ResolveFailure::classified(
            DnsFailure::Protocol,
            if public {
                "the native resolver public transaction space is reserved from HTTP-internal DNS"
            } else {
                "the native resolver transaction space is exhausted"
            },
        )
    })?;
    let wire = dns_wire::encode_query(id, &name, record_type).map_err(|_| {
        ResolveFailure::classified(DnsFailure::Protocol, "the DNS query could not be encoded")
    })?;
    Ok((
        id,
        PendingQuery {
            key,
            host: name,
            record_type,
            cname_hops,
            wire,
            attempts_sent: 0,
            attempt_limit: retry.attempt_limit,
            attempt_timeout: retry.attempt_timeout,
            servers_tried: 1,
            last_udp_generation: 0,
            next_attempt: Instant::now(),
            transport: QueryTransport::Udp,
            policy,
        },
    ))
}

fn allocate_id(
    pending: &HashMap<u16, PendingQuery>,
    next_id: &mut u16,
    public: bool,
) -> Option<u16> {
    if public && !public_txid_available(pending) {
        return None;
    }
    if pending.len() >= DNS_TRANSACTION_ID_SPACE {
        return None;
    }
    for _ in 0..=u16::MAX {
        let candidate = *next_id;
        *next_id = next_id.wrapping_add(1);
        if !pending.contains_key(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn public_txid_available(pending: &HashMap<u16, PendingQuery>) -> bool {
    let http_count = pending
        .values()
        .filter(|query| matches!(query.policy, QueryPolicy::Http(_)))
        .count();
    public_txid_available_counts(http_count, pending.len())
}

fn public_txid_available_counts(http_count: usize, total: usize) -> bool {
    if total >= DNS_TRANSACTION_ID_SPACE {
        return false;
    }
    let reserved_for_http = HTTP_DNS_TXID_RESERVE.saturating_sub(http_count);
    DNS_TRANSACTION_ID_SPACE - total > reserved_for_http
}

fn transmit_due(
    socket: &mut UdpSocket,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
    config: &ResolverConfig,
    poll: &mut NativePoll,
) -> bool {
    let mut config_dirty = false;
    let now = Instant::now();
    let due = state
        .pending
        .iter()
        .filter_map(|(id, query)| (query.next_attempt <= now).then_some(*id))
        .collect::<Vec<_>>();
    for id in due {
        let tcp_expired = state
            .pending
            .get(&id)
            .is_some_and(|query| matches!(query.transport, QueryTransport::Tcp(_)));
        if tcp_expired {
            if let Some(query) = remove_query(id, state, poll) {
                state.by_key.remove(&query.key);
                send_result(
                    results,
                    result_waker,
                    match query.policy {
                        QueryPolicy::Http(_) => ResolveResult::http(
                            query.key,
                            Err(ResolveFailure::classified(
                                DnsFailure::Io,
                                "the DNS-over-TCP fallback did not complete before its deadline",
                            )),
                        ),
                        QueryPolicy::Public(_) => ResolveResult::public(
                            query.key,
                            PublicLookupOutcome::Failed(Error::dns(
                                DnsFailure::Io,
                                "the DNS-over-TCP fallback did not complete before its deadline",
                            )),
                        ),
                    },
                );
            }
            continue;
        }
        let exhausted = state.pending.get(&id).is_some_and(|query| {
            matches!(query.transport, QueryTransport::Udp)
                && query.attempts_sent >= query.attempt_limit
        });
        if exhausted {
            let exhausted_generation = state
                .pending
                .get(&id)
                .map_or(0, |query| query.last_udp_generation);
            let may_try_another = state
                .pending
                .get(&id)
                .is_some_and(|query| query.servers_tried < config.nameservers.len());
            if may_try_another
                && advance_nameserver(socket, poll, config, &mut state.current_nameserver)
            {
                advance_udp_generation(&mut state.udp_generation);
                for query in state.pending.values_mut() {
                    if matches!(query.transport, QueryTransport::Udp) {
                        query.attempts_sent = 0;
                        query.servers_tried = query.servers_tried.saturating_add(1);
                        query.next_attempt = now;
                    }
                }
            } else {
                config_dirty = true;
                if exhausted_generation == state.udp_generation
                    && replace_current_nameserver(socket, poll, config, state.current_nameserver)
                {
                    advance_udp_generation(&mut state.udp_generation);
                }
                if let Some(query) = state.pending.remove(&id) {
                    state.by_key.remove(&query.key);
                    send_result(
                        results,
                        result_waker,
                        match query.policy {
                            QueryPolicy::Http(_) => ResolveResult::http(
                                query.key,
                                Err(ResolveFailure::classified(
                                    DnsFailure::NoNameserver,
                                    "the configured DNS servers did not answer within the retry budget",
                                )),
                            ),
                            QueryPolicy::Public(_) => ResolveResult::public(
                                query.key,
                                PublicLookupOutcome::Failed(Error::dns(
                                    DnsFailure::NoNameserver,
                                    "the configured DNS servers did not answer within the retry budget",
                                )),
                            ),
                        },
                    );
                }
                continue;
            }
        }
        let Some(query) = state.pending.get_mut(&id) else {
            continue;
        };
        if !matches!(query.transport, QueryTransport::Udp) {
            continue;
        }
        match socket.send(&query.wire) {
            Ok(written) if written == query.wire.len() => {
                query.attempts_sent += 1;
                query.last_udp_generation = state.udp_generation;
                query.next_attempt = now.checked_add(query.attempt_timeout).unwrap_or(now);
            }
            Ok(_) => {
                query.attempts_sent = query.attempt_limit;
                query.last_udp_generation = state.udp_generation;
                query.next_attempt = now;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                query.next_attempt = now.checked_add(Duration::from_millis(1)).unwrap_or(now);
            }
            Err(_) => {
                query.attempts_sent = query.attempt_limit;
                query.last_udp_generation = state.udp_generation;
                query.next_attempt = now;
            }
        }
    }
    config_dirty
}

fn advance_udp_generation(generation: &mut u64) {
    *generation = generation.wrapping_add(1);
    if *generation == 0 {
        *generation = 1;
    }
}

fn replace_current_nameserver(
    socket: &mut UdpSocket,
    poll: &mut NativePoll,
    config: &ResolverConfig,
    current: usize,
) -> bool {
    let Some(nameserver) = config.nameservers.get(current).copied() else {
        return false;
    };
    let Ok(mut replacement) = connect_one_nameserver(nameserver) else {
        return false;
    };
    if poll
        .register(&mut replacement, SOCKET_TOKEN, Interest::READABLE)
        .is_err()
    {
        return false;
    }
    let _deregister_result = poll.deregister(socket);
    *socket = replacement;
    true
}

fn advance_nameserver(
    socket: &mut UdpSocket,
    poll: &mut NativePoll,
    config: &ResolverConfig,
    current: &mut usize,
) -> bool {
    for offset in 1..config.nameservers.len() {
        let index = (*current + offset) % config.nameservers.len();
        let Ok(mut replacement) = connect_one_nameserver(config.nameservers[index]) else {
            continue;
        };
        if poll
            .register(&mut replacement, SOCKET_TOKEN, Interest::READABLE)
            .is_err()
        {
            continue;
        }
        let _deregister_result = poll.deregister(socket);
        *socket = replacement;
        *current = index;
        return true;
    }
    false
}

fn receive_packets(
    socket: &mut UdpSocket,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
    poll: &mut NativePoll,
    config: &ResolverConfig,
) -> bool {
    let mut buffer = [0_u8; DNS_PACKET_LIMIT];
    loop {
        let length = match socket.recv(&mut buffer) {
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return false,
            Err(error) if !udp_receive_error_poisons_generation(error.kind()) => continue,
            Err(_) => return true,
        };
        if length < 2 {
            continue;
        }
        let id = u16::from_be_bytes([buffer[0], buffer[1]]);
        let Some(query) = state.pending.get(&id) else {
            continue;
        };
        let remaining = MAX_CNAME_HOPS.saturating_sub(query.cname_hops);
        let result = parse_answer(
            &buffer[..length],
            id,
            &query.host,
            query.record_type,
            remaining,
        );
        let Some(result) = result else {
            continue;
        };
        let Some(mut query) = state.pending.remove(&id) else {
            continue;
        };
        if matches!(result, Ok(ParsedAnswer::Truncated)) {
            let attempt_timeout = query.attempt_timeout;
            match begin_tcp_fallback(
                id,
                &mut query,
                poll,
                config.nameservers[state.current_nameserver],
                attempt_timeout,
                &mut state.tcp_by_token,
                &mut state.next_tcp_token,
            ) {
                Ok(()) => {
                    state.pending.insert(id, query);
                }
                Err(failure) => {
                    state.by_key.remove(&query.key);
                    emit_failure(query, failure, results, result_waker);
                }
            }
            continue;
        }
        finish_answer(query, result, state, results, result_waker);
    }
}

fn udp_receive_error_poisons_generation(kind: io::ErrorKind) -> bool {
    !matches!(kind, io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted)
}

fn finish_answer(
    query: PendingQuery,
    result: Result<ParsedAnswer, ResolveFailure>,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    if matches!(query.policy, QueryPolicy::Public(_)) {
        finish_public_answer(query, result, state, results, result_waker);
        return;
    }
    finish_http_answer(query, result, state, results, result_waker);
}

fn follow_canonical(
    query: PendingQuery,
    target: DnsName,
    hops: u8,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    match prepare_name_query(
        query.key,
        target,
        query.record_type,
        query.cname_hops.saturating_add(hops),
        &state.pending,
        &mut state.next_id,
        query.policy.clone(),
    ) {
        Ok((next, replacement)) => {
            state.by_key.insert(query.key, next);
            state.pending.insert(next, replacement);
        }
        Err(failure) => {
            state.by_key.remove(&query.key);
            emit_failure(query, failure, results, result_waker);
        }
    }
}

fn finish_http_answer(
    query: PendingQuery,
    result: Result<ParsedAnswer, ResolveFailure>,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    match result {
        Ok(ParsedAnswer::Answer(answer)) => {
            state.by_key.remove(&query.key);
            if query.cname_hops == 0 {
                let now = Instant::now();
                state.cache.insert(
                    query.host.clone(),
                    Ok(answer.clone()),
                    answer.ttl,
                    MAX_POSITIVE_CACHE_TTL,
                    now,
                );
                let record = CachedFamily::Answer(answer.addresses.clone());
                let _expires = state.cache.insert_family(
                    query.host.clone(),
                    query.record_type,
                    record,
                    answer.ttl,
                    MAX_POSITIVE_CACHE_TTL,
                    now,
                );
            }
            send_result(
                results,
                result_waker,
                ResolveResult::http(query.key, Ok(answer)),
            );
        }
        Ok(ParsedAnswer::Canonical { target, hops }) => {
            follow_canonical(query, target, hops, state, results, result_waker);
        }
        Ok(ParsedAnswer::NoRecords(negative_ttl)) if query.record_type == DnsRecordType::A => {
            if query.cname_hops == 0 {
                if let Some(ttl) = negative_ttl {
                    let _expires = state.cache.insert_family(
                        query.host.clone(),
                        DnsRecordType::A,
                        CachedFamily::NoData,
                        ttl,
                        MAX_NEGATIVE_CACHE_TTL,
                        Instant::now(),
                    );
                }
            }
            match prepare_name_query(
                query.key,
                query.host.clone(),
                DnsRecordType::AAAA,
                query.cname_hops,
                &state.pending,
                &mut state.next_id,
                query.policy.clone(),
            ) {
                Ok((next, replacement)) => {
                    state.by_key.insert(query.key, next);
                    state.pending.insert(next, replacement);
                }
                Err(failure) => {
                    state.by_key.remove(&query.key);
                    emit_failure(query, failure, results, result_waker);
                }
            }
        }
        Ok(ParsedAnswer::NoRecords(negative_ttl)) => {
            state.by_key.remove(&query.key);
            let failure =
                ResolveFailure::new("the DNS response contained no usable A or AAAA records");
            if query.cname_hops == 0 {
                if let Some(ttl) = negative_ttl {
                    let now = Instant::now();
                    state.cache.insert(
                        query.host.clone(),
                        Err(failure.clone()),
                        ttl,
                        MAX_NEGATIVE_CACHE_TTL,
                        now,
                    );
                    let _expires = state.cache.insert_family(
                        query.host.clone(),
                        query.record_type,
                        CachedFamily::NoData,
                        ttl,
                        MAX_NEGATIVE_CACHE_TTL,
                        now,
                    );
                }
            }
            send_result(
                results,
                result_waker,
                ResolveResult::http(query.key, Err(failure)),
            );
        }
        Ok(ParsedAnswer::Negative(failure, negative_ttl)) => {
            state.by_key.remove(&query.key);
            if query.cname_hops == 0 {
                if let Some(ttl) = negative_ttl {
                    let now = Instant::now();
                    state.cache.insert(
                        query.host.clone(),
                        Err(failure.clone()),
                        ttl,
                        MAX_NEGATIVE_CACHE_TTL,
                        now,
                    );
                    for record_type in [DnsRecordType::A, DnsRecordType::AAAA] {
                        let _expires = state.cache.insert_family(
                            query.host.clone(),
                            record_type,
                            CachedFamily::NameNotFound,
                            ttl,
                            MAX_NEGATIVE_CACHE_TTL,
                            now,
                        );
                    }
                }
            }
            send_result(
                results,
                result_waker,
                ResolveResult::http(query.key, Err(failure)),
            );
        }
        Err(failure) => {
            state.by_key.remove(&query.key);
            emit_failure(query, failure, results, result_waker);
        }
        Ok(ParsedAnswer::Truncated) => {
            state.by_key.remove(&query.key);
            emit_failure(
                query,
                ResolveFailure::classified(
                    DnsFailure::Truncated,
                    "the DNS-over-TCP response was unexpectedly truncated",
                ),
                results,
                result_waker,
            );
        }
    }
}

fn finish_public_answer(
    query: PendingQuery,
    result: Result<ParsedAnswer, ResolveFailure>,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    let QueryPolicy::Public(session) = query.policy.clone() else {
        finish_http_answer(query, result, state, results, result_waker);
        return;
    };
    let mut session = *session;
    match result {
        Ok(ParsedAnswer::Canonical { target, hops }) => {
            follow_canonical(query, target, hops, state, results, result_waker);
        }
        Ok(ParsedAnswer::Truncated) => {
            state.by_key.remove(&query.key);
            emit_failure(
                query,
                ResolveFailure::classified(
                    DnsFailure::Truncated,
                    "the DNS-over-TCP response was unexpectedly truncated",
                ),
                results,
                result_waker,
            );
        }
        Err(failure) => {
            state.by_key.remove(&query.key);
            emit_failure(query, failure, results, result_waker);
        }
        Ok(ParsedAnswer::Answer(answer)) => {
            let mut addresses = answer.addresses;
            addresses.truncate(session.max_results);
            record_public_family(
                &mut session,
                PublicFamilyUpdate {
                    record_type: query.record_type,
                    status: ResolveStatus::Answer,
                    addresses,
                    ttl: Some(answer.ttl),
                    cacheable: query.cname_hops == 0,
                },
                &query.host,
                &mut state.cache,
            );
            continue_or_complete_public(query, session, state, results, result_waker);
        }
        Ok(ParsedAnswer::NoRecords(negative_ttl)) => {
            record_public_family(
                &mut session,
                PublicFamilyUpdate {
                    record_type: query.record_type,
                    status: ResolveStatus::NoData,
                    addresses: Vec::new(),
                    ttl: negative_ttl,
                    cacheable: query.cname_hops == 0,
                },
                &query.host,
                &mut state.cache,
            );
            continue_or_complete_public(query, session, state, results, result_waker);
        }
        Ok(ParsedAnswer::Negative(_failure, negative_ttl)) => {
            session.ipv4 = Some(FamilyOutcome {
                status: ResolveStatus::NameNotFound,
                addresses: Vec::new(),
                valid_until: None,
            });
            session.ipv6 = session.ipv4.clone();
            if query.cname_hops == 0 {
                if let Some(ttl) = negative_ttl {
                    let now = Instant::now();
                    if session.cache_mode != CacheMode::Bypass {
                        for record_type in [DnsRecordType::A, DnsRecordType::AAAA] {
                            if let Some(expires) = state.cache.insert_family(
                                query.host.clone(),
                                record_type,
                                CachedFamily::NameNotFound,
                                ttl,
                                MAX_NEGATIVE_CACHE_TTL,
                                now,
                            ) {
                                if let Some(outcome) = session.ipv4.as_mut() {
                                    outcome.valid_until = Some(expires);
                                }
                                if let Some(outcome) = session.ipv6.as_mut() {
                                    outcome.valid_until = Some(expires);
                                }
                            }
                        }
                    }
                }
            }
            let host = query.host.clone();
            after_candidate_families_complete(
                query.key,
                Some(query),
                session,
                &host,
                state,
                results,
                result_waker,
            );
        }
    }
}

struct PublicFamilyUpdate {
    record_type: DnsRecordType,
    status: ResolveStatus,
    addresses: Vec<IpAddr>,
    ttl: Option<Duration>,
    cacheable: bool,
}

fn record_public_family(
    session: &mut PublicSession,
    update: PublicFamilyUpdate,
    name: &DnsName,
    cache: &mut DnsCache,
) {
    let cached = match update.status {
        ResolveStatus::Answer => CachedFamily::Answer(update.addresses.clone()),
        ResolveStatus::NameNotFound => CachedFamily::NameNotFound,
        ResolveStatus::NoData => CachedFamily::NoData,
    };
    let valid_until = if update.cacheable && session.cache_mode != CacheMode::Bypass {
        update.ttl.and_then(|ttl| {
            let maximum = if update.status == ResolveStatus::Answer {
                MAX_POSITIVE_CACHE_TTL
            } else {
                MAX_NEGATIVE_CACHE_TTL
            };
            cache.insert_family(
                name.clone(),
                update.record_type,
                cached,
                ttl,
                maximum,
                Instant::now(),
            )
        })
    } else {
        None
    };
    let outcome = FamilyOutcome {
        status: update.status,
        addresses: update.addresses,
        valid_until,
    };
    match update.record_type {
        DnsRecordType::A => session.ipv4 = Some(outcome),
        DnsRecordType::AAAA => session.ipv6 = Some(outcome),
    }
}

fn continue_or_complete_public(
    query: PendingQuery,
    session: PublicSession,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    if let Some(record_type) = session_needs(&session) {
        match prepare_name_query(
            query.key,
            query.host.clone(),
            record_type,
            0,
            &state.pending,
            &mut state.next_id,
            QueryPolicy::Public(Box::new(session)),
        ) {
            Ok((next, replacement)) => {
                state.by_key.insert(query.key, next);
                state.pending.insert(next, replacement);
            }
            Err(failure) => {
                state.by_key.remove(&query.key);
                emit_failure(query, failure, results, result_waker);
            }
        }
        return;
    }
    let host = query.host.clone();
    after_candidate_families_complete(
        query.key,
        Some(query),
        session,
        &host,
        state,
        results,
        result_waker,
    );
}

fn after_candidate_families_complete(
    key: ResolveKey,
    query: Option<PendingQuery>,
    mut session: PublicSession,
    name: &DnsName,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    let (status, _, valid_until) = match session.family {
        AddressFamily::Ipv4 => family_response(session.ipv4.as_ref()),
        AddressFamily::Ipv6 => family_response(session.ipv6.as_ref()),
        AddressFamily::Both => combine_both(&session),
    };
    if session.cache_mode != CacheMode::Bypass {
        publish_http_view(&mut state.cache, name, &session, Instant::now());
    }
    if status == ResolveStatus::Answer {
        if let Some(query) = query {
            state.by_key.remove(&query.key);
        }
        send_result(
            results,
            result_waker,
            ResolveResult::public(key, complete_public_session(&session)),
        );
        return;
    }
    record_search_negative(&mut session, status, valid_until);
    let next_index = session.candidate_index.saturating_add(1);
    if let Some(next) = session.candidates.get(next_index).cloned() {
        session.candidate_index = next_index;
        session.candidate = next;
        if let Some(query) = query {
            state.by_key.remove(&query.key);
        }
        start_current_candidate(key, session, state, results, result_waker);
        return;
    }
    if let Some(query) = query {
        state.by_key.remove(&query.key);
    }
    send_result(
        results,
        result_waker,
        ResolveResult::public(key, complete_public_session(&session)),
    );
}

fn record_search_negative(
    session: &mut PublicSession,
    status: ResolveStatus,
    valid_until: Option<Instant>,
) {
    if status == ResolveStatus::NoData {
        session.saw_nodata = true;
    }
    session.negative_validity = Some(match (session.negative_validity, valid_until) {
        (None, until) => until,
        (Some(None), _) | (Some(_), None) => None,
        (Some(Some(previous)), Some(expires)) => Some(previous.min(expires)),
    });
}

fn publish_http_view(cache: &mut DnsCache, name: &DnsName, session: &PublicSession, now: Instant) {
    match session.family {
        AddressFamily::Ipv6 => {
            // IPv6-only public completions update the public AAAA family cache but never
            // mutate HTTP's coherent A-first name-level cache view.
        }
        AddressFamily::Ipv4 => match session.ipv4.as_ref() {
            Some(outcome) => publish_http_family(cache, name, outcome, now),
            None => cache.remove_http(name),
        },
        AddressFamily::Both => {
            let (status, addresses, valid_until) = combine_both(session);
            let ttl = valid_until.and_then(|expires| expires.checked_duration_since(now));
            match status {
                ResolveStatus::Answer => {
                    let Some(ttl) = ttl.filter(|ttl| !ttl.is_zero()) else {
                        cache.remove_http(name);
                        return;
                    };
                    let http_addresses = match session.ipv4.as_ref() {
                        Some(outcome) if outcome.status == ResolveStatus::Answer => {
                            outcome.addresses.clone()
                        }
                        _ => addresses,
                    };
                    cache.insert(
                        name.clone(),
                        Ok(ResolveAnswer {
                            addresses: http_addresses,
                            ttl,
                        }),
                        ttl,
                        MAX_POSITIVE_CACHE_TTL,
                        now,
                    );
                }
                ResolveStatus::NameNotFound => {
                    let Some(ttl) = ttl.filter(|ttl| !ttl.is_zero()) else {
                        cache.remove_http(name);
                        return;
                    };
                    cache.insert(
                        name.clone(),
                        Err(ResolveFailure::new("the DNS server returned NXDomain")),
                        ttl,
                        MAX_NEGATIVE_CACHE_TTL,
                        now,
                    );
                }
                ResolveStatus::NoData => {
                    let Some(ttl) = ttl.filter(|ttl| !ttl.is_zero()) else {
                        cache.remove_http(name);
                        return;
                    };
                    cache.insert(
                        name.clone(),
                        Err(ResolveFailure::new(
                            "the DNS response contained no usable A or AAAA records",
                        )),
                        ttl,
                        MAX_NEGATIVE_CACHE_TTL,
                        now,
                    );
                }
            }
        }
    }
}

fn publish_http_family(
    cache: &mut DnsCache,
    name: &DnsName,
    outcome: &FamilyOutcome,
    now: Instant,
) {
    match outcome.status {
        ResolveStatus::NoData => {
            cache.remove_http(name);
        }
        ResolveStatus::Answer => {
            let Some(ttl) = outcome
                .valid_until
                .and_then(|expires| expires.checked_duration_since(now))
                .filter(|ttl| !ttl.is_zero())
            else {
                cache.remove_http(name);
                return;
            };
            cache.insert(
                name.clone(),
                Ok(ResolveAnswer {
                    addresses: outcome.addresses.clone(),
                    ttl,
                }),
                ttl,
                MAX_POSITIVE_CACHE_TTL,
                now,
            );
        }
        ResolveStatus::NameNotFound => {
            let Some(ttl) = outcome
                .valid_until
                .and_then(|expires| expires.checked_duration_since(now))
                .filter(|ttl| !ttl.is_zero())
            else {
                cache.remove_http(name);
                return;
            };
            cache.insert(
                name.clone(),
                Err(ResolveFailure::new("the DNS server returned NXDomain")),
                ttl,
                MAX_NEGATIVE_CACHE_TTL,
                now,
            );
        }
    }
}

fn begin_tcp_fallback(
    id: u16,
    query: &mut PendingQuery,
    poll: &mut NativePoll,
    nameserver: SocketAddr,
    timeout: Duration,
    tcp_by_token: &mut HashMap<Token, u16>,
    next_tcp_token: &mut usize,
) -> Result<(), ResolveFailure> {
    let length = u16::try_from(query.wire.len()).map_err(|_| {
        ResolveFailure::classified(
            DnsFailure::Protocol,
            "the DNS query is too large for TCP framing",
        )
    })?;
    let token = Token(*next_tcp_token);
    *next_tcp_token = next_tcp_token.checked_add(1).ok_or_else(|| {
        ResolveFailure::classified(
            DnsFailure::Protocol,
            "the native resolver TCP token space is exhausted",
        )
    })?;
    let mut stream = TcpStream::connect(nameserver).map_err(|error| {
        ResolveFailure::classified(
            DnsFailure::Io,
            format!("DNS-over-TCP connect failed: {error}"),
        )
    })?;
    poll.register(
        &mut stream,
        token,
        Interest::READABLE.add(Interest::WRITABLE),
    )
    .map_err(|error| {
        ResolveFailure::classified(
            DnsFailure::Io,
            format!("DNS-over-TCP registration failed: {error}"),
        )
    })?;
    let mut outbound = Vec::with_capacity(query.wire.len() + 2);
    outbound.extend_from_slice(&length.to_be_bytes());
    outbound.extend_from_slice(&query.wire);
    query.next_attempt = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    query.transport = QueryTransport::Tcp(TcpFallback {
        stream,
        token,
        outbound,
        written: 0,
        inbound: Vec::new(),
        expected: None,
    });
    tcp_by_token.insert(token, id);
    Ok(())
}

fn remove_query(id: u16, state: &mut ResolverState, poll: &mut NativePoll) -> Option<PendingQuery> {
    let mut query = state.pending.remove(&id)?;
    if let QueryTransport::Tcp(tcp) = &mut query.transport {
        state.tcp_by_token.remove(&tcp.token);
        let _deregister_result = poll.deregister(&mut tcp.stream);
    }
    Some(query)
}

enum TcpDrive {
    Pending,
    Message(Vec<u8>),
}

fn receive_tcp(
    token: Token,
    readable: bool,
    writable: bool,
    poll: &mut NativePoll,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    let Some(id) = state.tcp_by_token.get(&token).copied() else {
        return;
    };
    let Some(mut query) = state.pending.remove(&id) else {
        state.tcp_by_token.remove(&token);
        return;
    };
    let drive = match &mut query.transport {
        QueryTransport::Tcp(tcp) => drive_tcp(tcp, readable, writable, poll),
        QueryTransport::Udp => {
            state.tcp_by_token.remove(&token);
            return;
        }
    };
    match drive {
        Ok(TcpDrive::Pending) => {
            state.pending.insert(id, query);
        }
        Ok(TcpDrive::Message(message)) => {
            if let QueryTransport::Tcp(tcp) = &mut query.transport {
                state.tcp_by_token.remove(&tcp.token);
                let _deregister_result = poll.deregister(&mut tcp.stream);
            }
            let remaining = MAX_CNAME_HOPS.saturating_sub(query.cname_hops);
            let parsed = parse_answer(&message, id, &query.host, query.record_type, remaining)
                .unwrap_or_else(|| {
                    Err(ResolveFailure::classified(
                        DnsFailure::Protocol,
                        "the DNS-over-TCP response did not match its query",
                    ))
                });
            finish_answer(query, parsed, state, results, result_waker);
        }
        Err(failure) => {
            if let QueryTransport::Tcp(tcp) = &mut query.transport {
                state.tcp_by_token.remove(&tcp.token);
                let _deregister_result = poll.deregister(&mut tcp.stream);
            }
            state.by_key.remove(&query.key);
            emit_failure(query, failure, results, result_waker);
        }
    }
}

fn drive_tcp(
    tcp: &mut TcpFallback,
    readable: bool,
    writable: bool,
    poll: &mut NativePoll,
) -> Result<TcpDrive, ResolveFailure> {
    if writable && tcp.written < tcp.outbound.len() {
        if let Some(error) = tcp.stream.take_error().map_err(|error| {
            ResolveFailure::classified(
                DnsFailure::Io,
                format!("DNS-over-TCP connect status failed: {error}"),
            )
        })? {
            return Err(ResolveFailure::classified(
                DnsFailure::Io,
                format!("DNS-over-TCP connect failed: {error}"),
            ));
        }
        while tcp.written < tcp.outbound.len() {
            match tcp.stream.write(&tcp.outbound[tcp.written..]) {
                Ok(0) => {
                    return Err(ResolveFailure::classified(
                        DnsFailure::Io,
                        "DNS-over-TCP closed while sending the query",
                    ));
                }
                Ok(written) => tcp.written += written,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    return Err(ResolveFailure::classified(
                        DnsFailure::Io,
                        format!("DNS-over-TCP query send failed: {error}"),
                    ));
                }
            }
        }
        if tcp.written == tcp.outbound.len() {
            poll.reregister(&mut tcp.stream, tcp.token, Interest::READABLE)
                .map_err(|error| {
                    ResolveFailure::classified(
                        DnsFailure::Io,
                        format!("DNS-over-TCP re-registration failed: {error}"),
                    )
                })?;
        }
    }
    if readable {
        let mut buffer = [0_u8; 1024];
        loop {
            match tcp.stream.read(&mut buffer) {
                Ok(0) => {
                    return Err(ResolveFailure::classified(
                        DnsFailure::Io,
                        "DNS-over-TCP closed before a complete response",
                    ));
                }
                Ok(read) => {
                    if tcp.inbound.len().saturating_add(read) > DNS_PACKET_LIMIT + 2 {
                        return Err(ResolveFailure::classified(
                            DnsFailure::Truncated,
                            "DNS-over-TCP response exceeds the private packet limit",
                        ));
                    }
                    tcp.inbound.extend_from_slice(&buffer[..read]);
                    if tcp.expected.is_none() && tcp.inbound.len() >= 2 {
                        let expected =
                            usize::from(u16::from_be_bytes([tcp.inbound[0], tcp.inbound[1]]));
                        if expected == 0 || expected > DNS_PACKET_LIMIT {
                            return Err(ResolveFailure::classified(
                                DnsFailure::Malformed,
                                "DNS-over-TCP response length is invalid",
                            ));
                        }
                        tcp.expected = Some(expected);
                    }
                    if let Some(expected) = tcp.expected {
                        if tcp.inbound.len() >= expected + 2 {
                            return Ok(TcpDrive::Message(tcp.inbound[2..expected + 2].to_vec()));
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    return Err(ResolveFailure::classified(
                        DnsFailure::Io,
                        format!("DNS-over-TCP response read failed: {error}"),
                    ));
                }
            }
        }
    }
    Ok(TcpDrive::Pending)
}

#[derive(Debug, Eq, PartialEq)]
enum ParsedAnswer {
    Answer(ResolveAnswer),
    Canonical { target: DnsName, hops: u8 },
    NoRecords(Option<Duration>),
    Negative(ResolveFailure, Option<Duration>),
    Truncated,
}

fn parse_answer(
    bytes: &[u8],
    expected_id: u16,
    expected_name: &DnsName,
    expected_type: DnsRecordType,
    remaining_cname_hops: u8,
) -> Option<Result<ParsedAnswer, ResolveFailure>> {
    let message = match dns_wire::parse_response(bytes) {
        Ok(message) => message,
        Err(_) => {
            return Some(Err(ResolveFailure::classified(
                DnsFailure::Malformed,
                "the DNS server returned a malformed message",
            )));
        }
    };
    if message.id != expected_id || !message.is_response {
        return None;
    }
    let expected_code = match expected_type {
        DnsRecordType::A => 1,
        DnsRecordType::AAAA => 28,
    };
    if message.questions.len() != 1
        || message.questions[0].name != *expected_name
        || message.questions[0].record_type != expected_code
    {
        return None;
    }
    if message.truncated {
        return Some(Ok(ParsedAnswer::Truncated));
    }
    let negative_ttl = message.authorities.iter().find_map(|record| {
        let WireRData::Soa { minimum } = record.data else {
            return None;
        };
        Some(Duration::from_secs(u64::from(record.ttl.min(minimum))))
    });
    if message.rcode == 3 {
        return Some(Ok(ParsedAnswer::Negative(
            ResolveFailure::new("the DNS server returned NXDomain"),
            negative_ttl,
        )));
    }
    if message.rcode != 0 {
        let class = match message.rcode {
            2 => DnsFailure::ServerFailure,
            5 => DnsFailure::Refused,
            1 => DnsFailure::Malformed,
            _ => DnsFailure::Unknown,
        };
        return Some(Err(ResolveFailure::classified(
            class,
            format!(
                "the DNS server returned {}",
                dns_response_code_debug(message.rcode)
            ),
        )));
    }

    let mut accepted_names = HashSet::from([expected_name.clone()]);
    let mut canonical_target = None;
    let mut ttl = u32::MAX;
    let mut hops = 0_u8;
    loop {
        let mut next_target = None;
        for answer in &message.answers {
            if !accepted_names.contains(&answer.name) {
                continue;
            }
            let WireRData::Cname(canonical) = &answer.data else {
                continue;
            };
            if canonical.is_root() {
                return Some(Err(ResolveFailure::classified(
                    DnsFailure::Protocol,
                    "the DNS CNAME target is the root name",
                )));
            }
            if accepted_names.contains(canonical) {
                continue;
            }
            next_target = Some((canonical.clone(), answer.ttl));
            break;
        }
        let Some((target, target_ttl)) = next_target else {
            break;
        };
        if hops >= remaining_cname_hops {
            return Some(Err(ResolveFailure::classified(
                DnsFailure::Protocol,
                "the DNS CNAME chain exceeds the private hop limit",
            )));
        }
        hops = hops.saturating_add(1);
        ttl = ttl.min(target_ttl);
        accepted_names.insert(target.clone());
        canonical_target = Some(target);
    }

    let mut addresses = Vec::new();
    for answer in &message.answers {
        if !accepted_names.contains(&answer.name) {
            continue;
        }
        match (expected_type, &answer.data) {
            (DnsRecordType::A, WireRData::A(address)) => {
                addresses.push(IpAddr::V4(*address));
            }
            (DnsRecordType::AAAA, WireRData::Aaaa(address)) => {
                addresses.push(IpAddr::V6(*address));
            }
            _ => continue,
        }
        ttl = ttl.min(answer.ttl);
    }
    if addresses.is_empty() {
        if let Some(canonical) = canonical_target {
            return Some(Ok(ParsedAnswer::Canonical {
                target: canonical,
                hops,
            }));
        }
        return Some(Ok(ParsedAnswer::NoRecords(negative_ttl)));
    }
    Some(Ok(ParsedAnswer::Answer(ResolveAnswer {
        addresses,
        ttl: Duration::from_secs(u64::from(ttl)),
    })))
}

fn dns_response_code_debug(code: u16) -> String {
    match code {
        0 => "NoError".to_owned(),
        1 => "FormErr".to_owned(),
        2 => "ServFail".to_owned(),
        3 => "NXDomain".to_owned(),
        4 => "NotImp".to_owned(),
        5 => "Refused".to_owned(),
        6 => "YXDomain".to_owned(),
        7 => "YXRRSet".to_owned(),
        8 => "NXRRSet".to_owned(),
        9 => "NotAuth".to_owned(),
        10 => "NotZone".to_owned(),
        16 => "BADVERS".to_owned(),
        17 => "BADKEY".to_owned(),
        18 => "BADTIME".to_owned(),
        19 => "BADMODE".to_owned(),
        20 => "BADNAME".to_owned(),
        21 => "BADALG".to_owned(),
        22 => "BADTRUNC".to_owned(),
        23 => "BADCOOKIE".to_owned(),
        other => format!("Unknown({other})"),
    }
}

#[cfg(any(test, fuzzing))]
pub(crate) fn fuzz_dns_response(data: &[u8]) {
    let Some((bytes, expected_id, expected_name, expected_type)) = fuzz_dns_input(data) else {
        return;
    };
    let first = parse_answer(
        &bytes,
        expected_id,
        &expected_name,
        expected_type,
        MAX_CNAME_HOPS,
    );
    let second = parse_answer(
        &bytes,
        expected_id,
        &expected_name,
        expected_type,
        MAX_CNAME_HOPS,
    );
    assert_eq!(first, second, "native DNS parsing retained hidden state");

    match &first {
        Some(Ok(ParsedAnswer::Answer(answer))) => {
            assert!(!answer.addresses.is_empty());
            assert!(answer.addresses.iter().all(|address| matches!(
                (expected_type, address),
                (DnsRecordType::A, IpAddr::V4(_)) | (DnsRecordType::AAAA, IpAddr::V6(_))
            )));
            assert!(answer.ttl <= Duration::from_secs(u64::from(u32::MAX)));
        }
        Some(Ok(ParsedAnswer::Canonical { target, hops })) => {
            assert!(!target.is_root());
            assert!(*hops >= 1);
        }
        Some(Ok(ParsedAnswer::NoRecords(ttl))) => {
            assert!(ttl.is_none_or(|ttl| ttl <= Duration::from_secs(u64::from(u32::MAX))));
        }
        Some(Ok(ParsedAnswer::Negative(failure, ttl))) => {
            assert!(!failure.message.is_empty());
            assert!(ttl.is_none_or(|ttl| ttl <= Duration::from_secs(u64::from(u32::MAX))));
        }
        Some(Err(failure)) => assert!(!failure.message.is_empty()),
        Some(Ok(ParsedAnswer::Truncated)) | None => {}
    }
}

#[cfg(any(test, fuzzing))]
fn fuzz_dns_input(data: &[u8]) -> Option<(Vec<u8>, u16, DnsName, DnsRecordType)> {
    const HEX_CASES: [(&[u8], u16, &str, DnsRecordType); 3] = [
        (
            b"HEX:A:expected:",
            0x1234,
            "expected.test.",
            DnsRecordType::A,
        ),
        (b"HEX:AAAA:ipv6:", 0x1235, "ipv6.test.", DnsRecordType::AAAA),
        (b"HEX:A:alias:", 0x1236, "alias.test.", DnsRecordType::A),
    ];
    for (prefix, id, name, record_type) in HEX_CASES {
        if let Some(hex) = data.strip_prefix(prefix) {
            let name = DnsName::from_ascii(name).ok()?;
            return Some((decode_dns_fuzz_hex(hex), id, name, record_type));
        }
    }
    if data.len() < 4 {
        return None;
    }
    let expected_id = u16::from_be_bytes([data[0], data[1]]);
    let expected_type = if data[2] & 1 == 0 {
        DnsRecordType::A
    } else {
        DnsRecordType::AAAA
    };
    let name = match data[3] % 3 {
        0 => "expected.test.",
        1 => "alias.test.",
        _ => "ipv6.test.",
    };
    Some((
        data[4..].iter().copied().take(DNS_PACKET_LIMIT).collect(),
        expected_id,
        DnsName::from_ascii(name).ok()?,
        expected_type,
    ))
}

#[cfg(any(test, fuzzing))]
fn decode_dns_fuzz_hex(hex: &[u8]) -> Vec<u8> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let mut decoded = Vec::with_capacity((hex.len() / 2).min(DNS_PACKET_LIMIT));
    for pair in hex.chunks_exact(2).take(DNS_PACKET_LIMIT) {
        let (Some(high), Some(low)) = (nibble(pair[0]), nibble(pair[1])) else {
            break;
        };
        decoded.push((high << 4) | low);
    }
    decoded
}

fn send_result(results: &Sender<ResolveResult>, waker: &NativeWaker, result: ResolveResult) {
    if results.send(result).is_ok() {
        let _wake_result = waker.wake();
    }
}

fn fail_all(
    pending: &mut HashMap<u16, PendingQuery>,
    by_key: &mut HashMap<ResolveKey, u16>,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
    message: &str,
) {
    for (_, query) in pending.drain() {
        emit_failure(
            query,
            ResolveFailure::classified(DnsFailure::Io, message),
            results,
            result_waker,
        );
    }
    by_key.clear();
}

fn resolver_internal(operation: &str, error: &io::Error) -> Error {
    Error::new(
        ErrorKind::Internal,
        format!("native resolver {operation} failed: {error}"),
    )
}

#[cfg(test)]
mod tests;
