#![cfg_attr(not(test), allow(dead_code))]

//! Private Engine-owned nonblocking DNS service.
//!
//! Hickory is used only for DNS wire encoding and decoding. NBReq owns the socket, poll loop,
//! retry clock, command/result queues, cancellation, wakeup, and joined shutdown.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, MessageType, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use mio::net::{TcpStream, UdpSocket};
use mio::{Interest, Token};

use super::native::NATIVE_SAFETY_POLL;
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct ResolveKey(pub(super) u64);

#[derive(Clone, Debug)]
pub(super) struct ResolverConfig {
    pub(super) nameservers: Vec<SocketAddr>,
    pub(super) search_suffixes: Vec<String>,
    pub(super) attempt_timeout: Duration,
    pub(super) attempts: u8,
}

impl ResolverConfig {
    pub(super) fn injected(nameserver: SocketAddr) -> Self {
        Self {
            nameservers: vec![nameserver],
            search_suffixes: Vec::new(),
            attempt_timeout: DEFAULT_ATTEMPT_TIMEOUT,
            attempts: DEFAULT_ATTEMPTS,
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
        })
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
        name: String,
        status: ResolveStatus,
        addresses: Vec<IpAddr>,
        valid_until: Option<Instant>,
        from_cache: bool,
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
    Shutdown,
}

#[derive(Clone)]
enum QueryPolicy {
    Http,
    Public(Box<PublicSession>),
}

#[derive(Clone)]
struct PublicSession {
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
}

#[derive(Clone)]
struct FamilyOutcome {
    status: ResolveStatus,
    addresses: Vec<IpAddr>,
    valid_until: Option<Instant>,
}

struct PendingQuery {
    key: ResolveKey,
    host: Name,
    record_type: RecordType,
    cname_hops: u8,
    wire: Vec<u8>,
    attempts_sent: u8,
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
    entries: HashMap<Name, CacheEntry>,
    family_entries: HashMap<(Name, RecordType), FamilyCacheEntry>,
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

    fn get(&mut self, name: &Name, now: Instant) -> Option<Result<ResolveAnswer, ResolveFailure>> {
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
        name: Name,
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
        name: &Name,
        record_type: RecordType,
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
        name: Name,
        record_type: RecordType,
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

    fn remove_http(&mut self, name: &Name) {
        self.entries.remove(name);
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
    waker: NativeWaker,
    joined: Option<JoinHandle<()>>,
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
        let state = ResolverState {
            pending: HashMap::new(),
            by_key: HashMap::new(),
            tcp_by_token: HashMap::new(),
            next_id: u16::from_ne_bytes(initial_id),
            next_tcp_token: FIRST_TCP_TOKEN,
            current_nameserver,
            udp_generation: 1,
            cache: DnsCache::new(),
            before_first_poll,
        };
        let joined = thread::Builder::new()
            .name("nbreq-native-dns".to_owned())
            .spawn(move || {
                resolver_main(
                    &mut poll,
                    &mut socket,
                    command_rx,
                    result_tx,
                    result_waker,
                    config,
                    state,
                );
            })
            .map_err(|error| resolver_internal("thread start", &error))?;
        Ok(Self {
            commands: command_tx,
            results: result_rx,
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
    socket: &mut UdpSocket,
    commands: Receiver<Command>,
    results: Sender<ResolveResult>,
    result_waker: NativeWaker,
    config: ResolverConfig,
    mut state: ResolverState,
) {
    loop {
        let mut stop = false;
        loop {
            match commands.try_recv() {
                Ok(Command::Resolve { key, host }) => {
                    if let Some(previous) = state.by_key.remove(&key) {
                        remove_query(previous, &mut state, poll);
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
                        RecordType::A,
                        0,
                        &state.pending,
                        &mut state.next_id,
                        QueryPolicy::Http,
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
                    begin_public_resolve(
                        key,
                        spec,
                        &config.search_suffixes,
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

        transmit_due(socket, &mut state, &results, &result_waker, &config, poll);
        let timeout = state
            .pending
            .values()
            .map(|query| query.next_attempt.saturating_duration_since(Instant::now()))
            .min()
            .map_or(NATIVE_SAFETY_POLL, |timeout| {
                timeout.min(NATIVE_SAFETY_POLL)
            });
        if let Some(barrier) = state.before_first_poll.take() {
            let _send_result = barrier.send(());
        }
        let mut targets = Vec::with_capacity(state.tcp_by_token.len() + 1);
        targets.push(PollTarget::new(SOCKET_TOKEN, &*socket, Interest::READABLE));
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
        if socket_ready
            && receive_packets(socket, &mut state, &results, &result_waker, poll, &config)
            && replace_current_nameserver(socket, poll, &config, state.current_nameserver)
        {
            advance_udp_generation(&mut state.udp_generation);
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

fn parse_dns_name(host: &str) -> Result<Name, ResolveFailure> {
    let mut name = Name::from_ascii(host).map_err(|_| {
        ResolveFailure::classified(DnsFailure::Protocol, "the DNS hostname is invalid")
    })?;
    name.set_fqdn(true);
    Ok(name)
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
        QueryPolicy::Http => send_result(
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

fn session_needs(session: &PublicSession) -> Option<RecordType> {
    match session.family {
        AddressFamily::Ipv4 if session.ipv4.is_none() => Some(RecordType::A),
        AddressFamily::Ipv6 if session.ipv6.is_none() => Some(RecordType::AAAA),
        AddressFamily::Both if session.ipv4.is_none() => Some(RecordType::A),
        AddressFamily::Both if session.ipv6.is_none() => Some(RecordType::AAAA),
        _ => None,
    }
}

fn begin_public_resolve(
    key: ResolveKey,
    spec: PublicResolveSpec,
    search_suffixes: &[String],
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    let mut session = PublicSession {
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
            if let Some((record, expires)) = state.cache.get_family(&name, RecordType::A, now) {
                session.ipv4 = Some(cached_family_outcome(record, expires));
            }
        }
        if matches!(family, AddressFamily::Ipv6 | AddressFamily::Both) {
            if let Some((record, expires)) = state.cache.get_family(&name, RecordType::AAAA, now) {
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
    let (status, mut addresses, valid_until) = match session.family {
        AddressFamily::Ipv4 => family_response(session.ipv4.as_ref()),
        AddressFamily::Ipv6 => family_response(session.ipv6.as_ref()),
        AddressFamily::Both => combine_both(session),
    };
    if addresses.len() > session.max_results {
        addresses.truncate(session.max_results);
    }
    let (status, valid_until, candidate_name) = if status == ResolveStatus::Answer {
        (status, valid_until, Some(session.candidate.clone()))
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
    PublicLookupOutcome::Completed {
        name: session.identity.clone(),
        status,
        addresses,
        valid_until,
        from_cache: session.from_cache,
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
    name: Name,
    record_type: RecordType,
    cname_hops: u8,
    pending: &HashMap<u16, PendingQuery>,
    next_id: &mut u16,
    policy: QueryPolicy,
) -> Result<(u16, PendingQuery), ResolveFailure> {
    let public = matches!(policy, QueryPolicy::Public(_));
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
    let mut message = Message::new();
    message
        .set_id(id)
        .set_recursion_desired(true)
        .add_query(Query::query(name.clone(), record_type));
    let wire = message.to_vec().map_err(|_| {
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
        .filter(|query| matches!(query.policy, QueryPolicy::Http))
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
) {
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
                        QueryPolicy::Http => ResolveResult::http(
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
            matches!(query.transport, QueryTransport::Udp) && query.attempts_sent >= config.attempts
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
                            QueryPolicy::Http => ResolveResult::http(
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
                query.next_attempt = now.checked_add(config.attempt_timeout).unwrap_or(now);
            }
            Ok(_) => {
                query.attempts_sent = config.attempts;
                query.last_udp_generation = state.udp_generation;
                query.next_attempt = now;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                query.next_attempt = now.checked_add(Duration::from_millis(1)).unwrap_or(now);
            }
            Err(_) => {
                query.attempts_sent = config.attempts;
                query.last_udp_generation = state.udp_generation;
                query.next_attempt = now;
            }
        }
    }
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
            match begin_tcp_fallback(
                id,
                &mut query,
                poll,
                config.nameservers[state.current_nameserver],
                config.attempt_timeout,
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
    target: Name,
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
        Ok(ParsedAnswer::NoRecords(negative_ttl)) if query.record_type == RecordType::A => {
            if query.cname_hops == 0 {
                if let Some(ttl) = negative_ttl {
                    let _expires = state.cache.insert_family(
                        query.host.clone(),
                        RecordType::A,
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
                RecordType::AAAA,
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
                    for record_type in [RecordType::A, RecordType::AAAA] {
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
                        for record_type in [RecordType::A, RecordType::AAAA] {
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
    record_type: RecordType,
    status: ResolveStatus,
    addresses: Vec<IpAddr>,
    ttl: Option<Duration>,
    cacheable: bool,
}

fn record_public_family(
    session: &mut PublicSession,
    update: PublicFamilyUpdate,
    name: &Name,
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
        RecordType::A => session.ipv4 = Some(outcome),
        RecordType::AAAA => session.ipv6 = Some(outcome),
        _ => {}
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
    name: &Name,
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

fn publish_http_view(cache: &mut DnsCache, name: &Name, session: &PublicSession, now: Instant) {
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

fn publish_http_family(cache: &mut DnsCache, name: &Name, outcome: &FamilyOutcome, now: Instant) {
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
    Canonical { target: Name, hops: u8 },
    NoRecords(Option<Duration>),
    Negative(ResolveFailure, Option<Duration>),
    Truncated,
}

fn parse_answer(
    bytes: &[u8],
    expected_id: u16,
    expected_name: &Name,
    expected_type: RecordType,
    remaining_cname_hops: u8,
) -> Option<Result<ParsedAnswer, ResolveFailure>> {
    let message = match Message::from_vec(bytes) {
        Ok(message) => message,
        Err(_) => {
            return Some(Err(ResolveFailure::classified(
                DnsFailure::Malformed,
                "the DNS server returned a malformed message",
            )));
        }
    };
    if message.id() != expected_id || message.message_type() != MessageType::Response {
        return None;
    }
    if message.queries().len() != 1
        || message.queries()[0].name() != expected_name
        || message.queries()[0].query_type() != expected_type
    {
        return None;
    }
    if message.truncated() {
        return Some(Ok(ParsedAnswer::Truncated));
    }
    let negative_ttl = message.name_servers().iter().find_map(|record| {
        let RData::SOA(soa) = record.data() else {
            return None;
        };
        Some(Duration::from_secs(u64::from(
            record.ttl().min(soa.minimum()),
        )))
    });
    if message.response_code() == ResponseCode::NXDomain {
        return Some(Ok(ParsedAnswer::Negative(
            ResolveFailure::new("the DNS server returned NXDomain"),
            negative_ttl,
        )));
    }
    if message.response_code() != ResponseCode::NoError {
        let class = match message.response_code() {
            ResponseCode::ServFail => DnsFailure::ServerFailure,
            ResponseCode::Refused => DnsFailure::Refused,
            ResponseCode::FormErr => DnsFailure::Malformed,
            _ => DnsFailure::Unknown,
        };
        return Some(Err(ResolveFailure::classified(
            class,
            format!("the DNS server returned {:?}", message.response_code()),
        )));
    }
    let mut accepted_names = HashSet::from([expected_name.clone()]);
    let mut canonical_target = None;
    let mut ttl = u32::MAX;
    let mut hops = 0_u8;
    loop {
        let mut next_target = None;
        for answer in message.answers() {
            if !accepted_names.contains(answer.name()) {
                continue;
            }
            let RData::CNAME(canonical) = answer.data() else {
                continue;
            };
            if canonical.0.is_root() {
                return Some(Err(ResolveFailure::classified(
                    DnsFailure::Protocol,
                    "the DNS CNAME target is the root name",
                )));
            }
            if accepted_names.contains(&canonical.0) {
                continue;
            }
            next_target = Some((canonical.0.clone(), answer.ttl()));
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
    for answer in message.answers() {
        if !accepted_names.contains(answer.name()) {
            continue;
        }
        match (expected_type, answer.data()) {
            (RecordType::A, RData::A(address)) => addresses.push(IpAddr::V4(address.0)),
            (RecordType::AAAA, RData::AAAA(address)) => addresses.push(IpAddr::V6(address.0)),
            _ => continue,
        }
        ttl = ttl.min(answer.ttl());
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
                (RecordType::A, IpAddr::V4(_)) | (RecordType::AAAA, IpAddr::V6(_))
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
fn fuzz_dns_input(data: &[u8]) -> Option<(Vec<u8>, u16, Name, RecordType)> {
    const HEX_CASES: [(&[u8], u16, &str, RecordType); 3] = [
        (b"HEX:A:expected:", 0x1234, "expected.test.", RecordType::A),
        (b"HEX:AAAA:ipv6:", 0x1235, "ipv6.test.", RecordType::AAAA),
        (b"HEX:A:alias:", 0x1236, "alias.test.", RecordType::A),
    ];
    for (prefix, id, name, record_type) in HEX_CASES {
        if let Some(hex) = data.strip_prefix(prefix) {
            let name = Name::from_ascii(name).ok()?;
            return Some((decode_dns_fuzz_hex(hex), id, name, record_type));
        }
    }
    if data.len() < 4 {
        return None;
    }
    let expected_id = u16::from_be_bytes([data[0], data[1]]);
    let expected_type = if data[2] & 1 == 0 {
        RecordType::A
    } else {
        RecordType::AAAA
    };
    let name = match data[3] % 3 {
        0 => "expected.test.",
        1 => "alias.test.",
        _ => "ipv6.test.",
    };
    Some((
        data[4..].iter().copied().take(DNS_PACKET_LIMIT).collect(),
        expected_id,
        Name::from_ascii(name).ok()?,
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
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::net::UdpSocket as StdUdpSocket;
    use std::sync::mpsc as test_channel;

    use hickory_proto::rr::rdata::{A, AAAA, CNAME, SOA};
    use hickory_proto::rr::{RData, Record};
    use rcgen::{CertificateParams, KeyPair};
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::{ServerConfig, ServerConnection, StreamOwned};

    use super::*;
    use crate::backend::native::NativeReactor;
    use crate::backend::native_tls::TLS_FLIGHT_LIMIT;

    #[test]
    fn public_transaction_ids_leave_an_http_reserve() {
        assert!(public_txid_available_counts(0, 0));
        assert!(public_txid_available_counts(
            0,
            DNS_TRANSACTION_ID_SPACE - HTTP_DNS_TXID_RESERVE - 1
        ));
        assert!(!public_txid_available_counts(
            0,
            DNS_TRANSACTION_ID_SPACE - HTTP_DNS_TXID_RESERVE
        ));
        assert!(public_txid_available_counts(
            HTTP_DNS_TXID_RESERVE,
            HTTP_DNS_TXID_RESERVE
        ));
        assert!(public_txid_available_counts(
            HTTP_DNS_TXID_RESERVE,
            DNS_TRANSACTION_ID_SPACE - 1
        ));
        assert!(!public_txid_available_counts(
            HTTP_DNS_TXID_RESERVE,
            DNS_TRANSACTION_ID_SPACE
        ));
        assert!(!public_txid_available_counts(0, DNS_TRANSACTION_ID_SPACE));
    }

    #[test]
    fn public_search_candidates_follow_fq10_exact_and_suffix_placement() {
        let suffixes = ["corp.test", "lab.test"];
        assert_eq!(
            public_search_candidates("www", false, &suffixes.map(str::to_owned)),
            vec!["www".to_owned()]
        );
        assert_eq!(
            public_search_candidates("www", true, &suffixes.map(str::to_owned)),
            vec!["www.corp.test".to_owned(), "www.lab.test".to_owned()]
        );
        assert_eq!(
            public_search_candidates("www.svc", true, &suffixes.map(str::to_owned)),
            vec![
                "www.svc".to_owned(),
                "www.svc.corp.test".to_owned(),
                "www.svc.lab.test".to_owned()
            ]
        );
        assert_eq!(
            public_search_candidates("www", true, &[] as &[String]),
            vec!["www".to_owned()]
        );
        let long_suffix = "a".repeat(250);
        assert_eq!(
            public_search_candidates("www", true, &[long_suffix]),
            vec!["www".to_owned()]
        );
    }

    #[test]
    fn checked_in_dns_fuzz_seeds_reach_the_policy_parser() {
        for seed in [
            include_bytes!("../../fuzz/corpus/native_dns_response/a.seed").as_slice(),
            include_bytes!("../../fuzz/corpus/native_dns_response/aaaa.seed").as_slice(),
            include_bytes!("../../fuzz/corpus/native_dns_response/cname.seed").as_slice(),
            include_bytes!("../../fuzz/corpus/native_dns_response/nxdomain.seed").as_slice(),
            include_bytes!("../../fuzz/corpus/native_dns_response/root-cname.seed").as_slice(),
            include_bytes!("../../fuzz/corpus/native_dns_response/truncated.seed").as_slice(),
        ] {
            let (bytes, id, name, record_type) =
                fuzz_dns_input(seed).expect("DNS fuzz seed must decode");
            assert!(
                parse_answer(&bytes, id, &name, record_type, MAX_CNAME_HOPS).is_some(),
                "DNS fuzz seed must reach an expected response result"
            );
            fuzz_dns_response(seed);
        }
    }
    use crate::{
        Completion, EngineConfig, ErrorKind, ExecuteError, LimitKind, Request, StreamRequest,
        TlsVerification, TransportStage, UploadBody,
    };

    struct DnsFixture {
        address: SocketAddr,
        stop: Sender<()>,
        joined: Option<JoinHandle<()>>,
    }

    struct ScriptedDnsFixture {
        address: SocketAddr,
        joined: Option<JoinHandle<()>>,
    }

    struct SourceGenerationDnsFixture {
        address: SocketAddr,
        stop: Sender<()>,
        observed: Arc<Mutex<Vec<(Name, SocketAddr)>>>,
        joined: Option<JoinHandle<()>>,
    }

    impl ScriptedDnsFixture {
        fn new(
            request_count: usize,
            mut handler: impl FnMut(Message) -> Message + Send + 'static,
        ) -> Self {
            let socket = StdUdpSocket::bind("127.0.0.1:0").expect("scripted DNS must bind");
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("scripted DNS timeout must configure");
            let address = socket.local_addr().expect("scripted DNS address");
            let joined = thread::spawn(move || {
                let mut buffer = [0_u8; DNS_PACKET_LIMIT];
                for _ in 0..request_count {
                    let (length, peer) = socket
                        .recv_from(&mut buffer)
                        .expect("scripted DNS request must arrive");
                    let request = Message::from_vec(&buffer[..length])
                        .expect("scripted DNS request must parse");
                    let response = handler(request);
                    let wire = response
                        .to_vec()
                        .expect("scripted DNS response must encode");
                    socket
                        .send_to(&wire, peer)
                        .expect("scripted DNS response must send");
                }
            });
            Self {
                address,
                joined: Some(joined),
            }
        }
    }

    impl Drop for ScriptedDnsFixture {
        fn drop(&mut self) {
            if let Some(joined) = self.joined.take() {
                joined.join().expect("scripted DNS fixture must join");
            }
        }
    }

    impl SourceGenerationDnsFixture {
        fn answering_new_source_ports(address: Ipv4Addr) -> Self {
            let socket =
                StdUdpSocket::bind("127.0.0.1:0").expect("source-generation DNS fixture must bind");
            socket
                .set_read_timeout(Some(Duration::from_millis(25)))
                .expect("source-generation DNS timeout must configure");
            let fixture_address = socket
                .local_addr()
                .expect("source-generation DNS fixture address");
            let (stop_tx, stop_rx) = test_channel::channel();
            let observed = Arc::new(Mutex::new(Vec::new()));
            let thread_observed = Arc::clone(&observed);
            let joined = thread::spawn(move || {
                let mut first_peer = None;
                let mut buffer = [0_u8; DNS_PACKET_LIMIT];
                loop {
                    if stop_rx.try_recv().is_ok() {
                        return;
                    }
                    let (length, peer) = match socket.recv_from(&mut buffer) {
                        Ok(received) => received,
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) =>
                        {
                            continue;
                        }
                        Err(error) => {
                            panic!("source-generation DNS fixture receive failed: {error}")
                        }
                    };
                    let request = Message::from_vec(&buffer[..length])
                        .expect("source-generation DNS query must parse");
                    let query = request
                        .query()
                        .expect("source-generation DNS query must exist")
                        .clone();
                    thread_observed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((query.name().clone(), peer));
                    match first_peer {
                        None => first_peer = Some(peer),
                        Some(poisoned_peer) if peer == poisoned_peer => {
                            // The first query proves this generation initially worked. All later
                            // packets from that source port model a route/socket generation that
                            // became silently unusable after an adapter transition.
                            continue;
                        }
                        Some(_) => {}
                    }
                    let mut response = Message::new();
                    response
                        .set_id(request.id())
                        .set_message_type(MessageType::Response)
                        .set_recursion_available(true)
                        .add_query(query.clone())
                        .add_answer(Record::from_rdata(
                            query.name().clone(),
                            60,
                            RData::A(A(address)),
                        ));
                    let wire = response
                        .to_vec()
                        .expect("source-generation DNS response must encode");
                    socket
                        .send_to(&wire, peer)
                        .expect("source-generation DNS response must send");
                }
            });
            Self {
                address: fixture_address,
                stop: stop_tx,
                observed,
                joined: Some(joined),
            }
        }

        fn observations(&self) -> Vec<(Name, SocketAddr)> {
            self.observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl Drop for SourceGenerationDnsFixture {
        fn drop(&mut self) {
            let _send_result = self.stop.send(());
            if let Some(joined) = self.joined.take() {
                joined
                    .join()
                    .expect("source-generation DNS fixture must join");
            }
        }
    }

    impl DnsFixture {
        fn answering(address: Ipv4Addr) -> Self {
            let socket = StdUdpSocket::bind("127.0.0.1:0").expect("DNS fixture must bind");
            socket
                .set_read_timeout(Some(Duration::from_millis(25)))
                .expect("DNS fixture timeout must configure");
            let fixture_address = socket.local_addr().expect("DNS fixture address");
            let (stop_tx, stop_rx) = test_channel::channel();
            let joined = thread::spawn(move || {
                let mut buffer = [0_u8; DNS_PACKET_LIMIT];
                loop {
                    if stop_rx.try_recv().is_ok() {
                        return;
                    }
                    let (length, peer) = match socket.recv_from(&mut buffer) {
                        Ok(received) => received,
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) =>
                        {
                            continue;
                        }
                        Err(error) => panic!("DNS fixture receive failed: {error}"),
                    };
                    let request =
                        Message::from_vec(&buffer[..length]).expect("DNS fixture query must parse");
                    let query = request
                        .query()
                        .expect("DNS fixture query must exist")
                        .clone();
                    let mut response = Message::new();
                    response
                        .set_id(request.id())
                        .set_message_type(MessageType::Response)
                        .set_recursion_available(true)
                        .add_query(query.clone())
                        .add_answer(Record::from_rdata(
                            query.name().clone(),
                            60,
                            RData::A(A(address)),
                        ));
                    let wire = response.to_vec().expect("DNS fixture response must encode");
                    socket
                        .send_to(&wire, peer)
                        .expect("DNS fixture response must send");
                }
            });
            Self {
                address: fixture_address,
                stop: stop_tx,
                joined: Some(joined),
            }
        }
    }

    impl Drop for DnsFixture {
        fn drop(&mut self) {
            let _send_result = self.stop.send(());
            if let Some(joined) = self.joined.take() {
                joined.join().expect("DNS fixture must join");
            }
        }
    }

    fn fqdn(name: &str) -> Name {
        let mut name = Name::from_ascii(name).expect("fixture DNS name must parse");
        name.set_fqdn(true);
        name
    }

    #[test]
    fn production_query_encoder_has_one_question_and_no_records() {
        let name = fqdn("bounded-query.test");
        let mut next_id = 41;
        let (id, query) = prepare_name_query(
            ResolveKey(9),
            name.clone(),
            RecordType::A,
            0,
            &HashMap::new(),
            &mut next_id,
            QueryPolicy::Http,
        )
        .expect("ordinary DNS query must encode");
        let message = Message::from_vec(&query.wire).expect("ordinary DNS query must decode");

        assert_eq!(message.id(), id);
        assert_eq!(message.queries().len(), 1);
        assert_eq!(message.queries()[0].name(), &name);
        assert_eq!(message.queries()[0].query_type(), RecordType::A);
        assert!(message.answers().is_empty());
        assert!(message.name_servers().is_empty());
        assert!(message.additionals().is_empty());
    }

    #[test]
    fn interrupted_udp_receive_does_not_poison_the_transport_generation() {
        assert!(!udp_receive_error_poisons_generation(
            io::ErrorKind::WouldBlock
        ));
        assert!(!udp_receive_error_poisons_generation(
            io::ErrorKind::Interrupted
        ));
        assert!(udp_receive_error_poisons_generation(
            io::ErrorKind::ConnectionReset
        ));
        assert!(udp_receive_error_poisons_generation(
            io::ErrorKind::NetworkUnreachable
        ));
    }

    #[test]
    fn parser_accepts_bounded_cname_chain_and_aaaa_answers() {
        let alias = fqdn("alias.test");
        let canonical = fqdn("canonical.test");
        let mut cname_response = Message::new();
        cname_response
            .set_id(41)
            .set_message_type(MessageType::Response)
            .add_query(Query::query(alias.clone(), RecordType::A))
            .add_answer(Record::from_rdata(
                alias.clone(),
                30,
                RData::CNAME(CNAME(canonical.clone())),
            ))
            .add_answer(Record::from_rdata(
                canonical,
                20,
                RData::A(A(Ipv4Addr::new(127, 0, 0, 9))),
            ));
        let cname_wire = cname_response.to_vec().expect("CNAME response must encode");
        let Some(Ok(ParsedAnswer::Answer(answer))) =
            parse_answer(&cname_wire, 41, &alias, RecordType::A, MAX_CNAME_HOPS)
        else {
            panic!("CNAME response must resolve");
        };
        assert_eq!(
            answer.addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 9))]
        );
        assert_eq!(answer.ttl, Duration::from_secs(20));

        let ipv6_name = fqdn("ipv6.test");
        let ipv6 = Ipv6Addr::LOCALHOST;
        let mut aaaa_response = Message::new();
        aaaa_response
            .set_id(42)
            .set_message_type(MessageType::Response)
            .add_query(Query::query(ipv6_name.clone(), RecordType::AAAA))
            .add_answer(Record::from_rdata(
                ipv6_name.clone(),
                60,
                RData::AAAA(AAAA(ipv6)),
            ));
        let aaaa_wire = aaaa_response.to_vec().expect("AAAA response must encode");
        let Some(Ok(ParsedAnswer::Answer(answer))) =
            parse_answer(&aaaa_wire, 42, &ipv6_name, RecordType::AAAA, MAX_CNAME_HOPS)
        else {
            panic!("AAAA response must resolve");
        };
        assert_eq!(answer.addresses, vec![IpAddr::V6(ipv6)]);
    }

    #[test]
    fn parser_rejects_a_root_cname_target() {
        let alias = fqdn("alias.test");
        let mut response = Message::new();
        response
            .set_id(43)
            .set_message_type(MessageType::Response)
            .add_query(Query::query(alias.clone(), RecordType::A))
            .add_answer(Record::from_rdata(
                alias.clone(),
                30,
                RData::CNAME(CNAME(Name::root())),
            ));
        let wire = response.to_vec().expect("root-CNAME response must encode");

        let Some(Err(failure)) = parse_answer(&wire, 43, &alias, RecordType::A, MAX_CNAME_HOPS)
        else {
            panic!("a root CNAME target must fail before resolver follow-up");
        };
        assert_eq!(failure.message, "the DNS CNAME target is the root name");
    }

    fn cname_chain_wire(id: u16, start: &Name, hops: u8, address: Option<Ipv4Addr>) -> Vec<u8> {
        let mut response = Message::new();
        response
            .set_id(id)
            .set_message_type(MessageType::Response)
            .add_query(Query::query(start.clone(), RecordType::A));
        let mut current = start.clone();
        for index in 1..=hops {
            let next = fqdn(&format!("h{index}.limit.test"));
            response.add_answer(Record::from_rdata(
                current,
                30,
                RData::CNAME(CNAME(next.clone())),
            ));
            current = next;
        }
        if let Some(ip) = address {
            response.add_answer(Record::from_rdata(current, 20, RData::A(A(ip))));
        }
        response.to_vec().expect("CNAME chain must encode")
    }

    #[test]
    fn parser_counts_in_message_cname_links_against_the_remaining_budget() {
        let start = fqdn("start.limit.test");
        let over = cname_chain_wire(61, &start, MAX_CNAME_HOPS + 1, None);
        let Some(Err(failure)) = parse_answer(&over, 61, &start, RecordType::A, MAX_CNAME_HOPS)
        else {
            panic!("nine in-message CNAME links must exceed the hop budget");
        };
        assert_eq!(
            failure.message,
            "the DNS CNAME chain exceeds the private hop limit"
        );

        let exact = cname_chain_wire(62, &start, MAX_CNAME_HOPS, None);
        let Some(Ok(ParsedAnswer::Canonical { hops, .. })) =
            parse_answer(&exact, 62, &start, RecordType::A, MAX_CNAME_HOPS)
        else {
            panic!("eight in-message CNAME links must leave a follow-up target");
        };
        assert_eq!(hops, MAX_CNAME_HOPS);

        let answered = cname_chain_wire(
            63,
            &start,
            MAX_CNAME_HOPS,
            Some(Ipv4Addr::new(127, 0, 0, 63)),
        );
        let Some(Ok(ParsedAnswer::Answer(answer))) =
            parse_answer(&answered, 63, &start, RecordType::A, MAX_CNAME_HOPS)
        else {
            panic!("eight in-message CNAME links plus an address must complete");
        };
        assert_eq!(
            answer.addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 63))]
        );

        let leftover = cname_chain_wire(64, &start, 3, None);
        let Some(Err(failure)) = parse_answer(&leftover, 64, &start, RecordType::A, 2) else {
            panic!("three links must exceed a remaining budget of two");
        };
        assert_eq!(
            failure.message,
            "the DNS CNAME chain exceeds the private hop limit"
        );
    }

    #[test]
    fn parser_marks_truncation_and_ignores_wrong_question() {
        let name = fqdn("expected.test");
        let mut truncated = Message::new();
        truncated
            .set_id(51)
            .set_message_type(MessageType::Response)
            .set_truncated(true)
            .add_query(Query::query(name.clone(), RecordType::A));
        let wire = truncated.to_vec().expect("truncated response must encode");
        assert!(matches!(
            parse_answer(&wire, 51, &name, RecordType::A, MAX_CNAME_HOPS),
            Some(Ok(ParsedAnswer::Truncated))
        ));

        let mut wrong = Message::new();
        wrong
            .set_id(52)
            .set_message_type(MessageType::Response)
            .add_query(Query::query(fqdn("other.test"), RecordType::A));
        let wire = wrong.to_vec().expect("wrong response must encode");
        assert!(parse_answer(&wire, 52, &name, RecordType::A, MAX_CNAME_HOPS).is_none());
    }

    fn accept_before(listener: &TcpListener, deadline: Instant) -> std::net::TcpStream {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("accepted DNS TCP fixture must be blocking");
                    return stream;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "DNS TCP fallback did not connect"
                    );
                    thread::yield_now();
                }
                Err(error) => panic!("DNS TCP fixture accept failed: {error}"),
            }
        }
    }

    fn bind_dns_tcp_udp_pair(context: &str) -> (TcpListener, StdUdpSocket, SocketAddr) {
        for attempt in 0..1024 {
            // The Windows TCP and UDP ephemeral allocators are independent. Under a parallel test
            // run either protocol can repeatedly choose a number that the other protocol already
            // owns. Try both reservation orders and yield periodically; the test needs one shared
            // numeric port, not a particular ephemeral allocation policy.
            if attempt % 2 == 0 {
                let udp = StdUdpSocket::bind("127.0.0.1:0")
                    .unwrap_or_else(|error| panic!("{context}: UDP bind failed: {error}"));
                let address = udp
                    .local_addr()
                    .unwrap_or_else(|error| panic!("{context}: UDP address failed: {error}"));
                match TcpListener::bind(address) {
                    Ok(listener) => return (listener, udp, address),
                    Err(error) if attempt == 1023 => panic!(
                        "{context}: could not reserve one numeric port for both TCP and UDP; last TCP bind error: {error}"
                    ),
                    Err(_) => {}
                }
            } else {
                let listener = TcpListener::bind("127.0.0.1:0")
                    .unwrap_or_else(|error| panic!("{context}: TCP bind failed: {error}"));
                let address = listener
                    .local_addr()
                    .unwrap_or_else(|error| panic!("{context}: TCP address failed: {error}"));
                match StdUdpSocket::bind(address) {
                    Ok(udp) => return (listener, udp, address),
                    Err(error) if attempt == 1023 => panic!(
                        "{context}: could not reserve one numeric port for both TCP and UDP; last UDP bind error: {error}"
                    ),
                    Err(_) => {}
                }
            }
            if attempt % 32 == 31 {
                thread::sleep(Duration::from_millis(1));
            }
        }
        unreachable!("the final reservation attempt either returns or panics")
    }

    #[test]
    fn truncated_udp_response_falls_back_to_fragmented_tcp() {
        let (listener, udp, address) = bind_dns_tcp_udp_pair("DNS fallback fixture");
        listener
            .set_nonblocking(true)
            .expect("DNS TCP listener must be nonblocking");
        udp.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("DNS UDP fixture timeout");
        let joined = thread::spawn(move || {
            let mut buffer = [0_u8; DNS_PACKET_LIMIT];
            let (length, peer) = udp
                .recv_from(&mut buffer)
                .expect("initial DNS UDP query must arrive");
            let request = Message::from_vec(&buffer[..length]).expect("UDP query must parse");
            let query = request.query().expect("UDP query must exist").clone();
            let mut truncated = Message::new();
            truncated
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .set_truncated(true)
                .add_query(query.clone());
            udp.send_to(
                &truncated.to_vec().expect("truncated reply must encode"),
                peer,
            )
            .expect("truncated reply must send");

            let mut stream = accept_before(&listener, Instant::now() + Duration::from_secs(2));
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("DNS TCP read timeout");
            let mut length = [0_u8; 2];
            stream
                .read_exact(&mut length)
                .expect("DNS TCP query length must arrive");
            let mut wire = vec![0_u8; usize::from(u16::from_be_bytes(length))];
            stream
                .read_exact(&mut wire)
                .expect("DNS TCP query must arrive");
            let tcp_request = Message::from_vec(&wire).expect("DNS TCP query must parse");
            assert_eq!(tcp_request.id(), request.id());
            assert_eq!(tcp_request.query(), Some(&query));

            let mut response = Message::new();
            response
                .set_id(tcp_request.id())
                .set_message_type(MessageType::Response)
                .add_query(query.clone())
                .add_answer(Record::from_rdata(
                    query.name().clone(),
                    45,
                    RData::A(A(Ipv4Addr::new(127, 0, 0, 21))),
                ));
            let wire = response.to_vec().expect("DNS TCP response must encode");
            let frame_length = u16::try_from(wire.len())
                .expect("DNS TCP fixture response length")
                .to_be_bytes();
            stream
                .write_all(&frame_length[..1])
                .expect("first length fragment must send");
            thread::yield_now();
            stream
                .write_all(&[&frame_length[1..], &wire[..1]].concat())
                .expect("second DNS TCP fragment must send");
            thread::yield_now();
            stream
                .write_all(&wire[1..])
                .expect("final DNS TCP fragment must send");
        });

        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver = NativeResolver::new(ResolverConfig::for_test(address), owner.waker())
            .expect("TCP-fallback resolver must construct");
        resolver
            .resolve(ResolveKey(70), "tcp-fallback.test".to_owned())
            .expect("TCP-fallback resolution must submit");
        let answer = wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("TCP fallback must resolve");
        assert_eq!(
            answer.addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 21))]
        );
        resolver.shutdown().expect("TCP resolver must join");
        joined.join().expect("DNS TCP fixture must join");
    }

    #[test]
    fn cancellation_closes_an_active_dns_tcp_fallback() {
        let (listener, udp, address) = bind_dns_tcp_udp_pair("DNS cancellation fixture");
        listener
            .set_nonblocking(true)
            .expect("DNS TCP barrier must be nonblocking");
        udp.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("DNS UDP barrier timeout");
        let (accepted_tx, accepted_rx) = test_channel::channel();
        let joined = thread::spawn(move || {
            let mut buffer = [0_u8; DNS_PACKET_LIMIT];
            let (length, peer) = udp
                .recv_from(&mut buffer)
                .expect("DNS UDP barrier query must arrive");
            let request = Message::from_vec(&buffer[..length]).expect("barrier query must parse");
            let mut truncated = Message::new();
            truncated
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .set_truncated(true)
                .add_query(request.query().expect("barrier query").clone());
            udp.send_to(
                &truncated.to_vec().expect("barrier response must encode"),
                peer,
            )
            .expect("barrier response must send");
            let mut stream = accept_before(&listener, Instant::now() + Duration::from_secs(2));
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("DNS TCP barrier read timeout");
            accepted_tx.send(()).expect("accept signal must send");
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        panic!("cancelled DNS TCP socket remained open")
                    }
                    Err(error) => panic!("DNS TCP barrier read failed: {error}"),
                }
            }
        });

        let owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver = NativeResolver::new(ResolverConfig::for_test(address), owner.waker())
            .expect("TCP barrier resolver must construct");
        resolver
            .resolve(ResolveKey(71), "tcp-cancel.test".to_owned())
            .expect("TCP barrier resolution must submit");
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("resolver must enter DNS TCP fallback");
        let started = Instant::now();
        resolver
            .cancel(ResolveKey(71))
            .expect("DNS TCP cancellation must wake");
        resolver.shutdown().expect("DNS TCP resolver must join");
        assert!(started.elapsed() < Duration::from_millis(500));
        joined.join().expect("DNS TCP barrier must observe close");
    }

    fn wait_for_resolution(owner: &mut NativeReactor, resolver: &NativeResolver) -> ResolveResult {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let results = resolver.drain().expect("resolver results must remain live");
            if let Some(result) = results.into_iter().next() {
                return result;
            }
            assert!(Instant::now() < deadline, "scripted resolution timed out");
            owner.poll(deadline).expect("owner wake poll must succeed");
        }
    }

    fn wait_for_resolutions(
        owner: &mut NativeReactor,
        resolver: &NativeResolver,
        expected: usize,
    ) -> Vec<ResolveResult> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut results = Vec::with_capacity(expected);
        loop {
            results.extend(resolver.drain().expect("resolver results must remain live"));
            if results.len() == expected {
                return results;
            }
            assert!(
                results.len() < expected,
                "resolver returned more terminals than accepted lookups"
            );
            assert!(Instant::now() < deadline, "scripted resolutions timed out");
            owner.poll(deadline).expect("owner wake poll must succeed");
        }
    }

    #[test]
    fn resolver_follows_cname_and_falls_back_from_a_to_aaaa() {
        let alias = fqdn("alias-only.test");
        let canonical = fqdn("canonical-only.test");
        let cname_fixture = ScriptedDnsFixture::new(2, move |request| {
            let query = request.query().expect("CNAME query must exist").clone();
            let mut response = Message::new();
            response
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .add_query(query.clone());
            if query.name() == &alias {
                response.add_answer(Record::from_rdata(
                    alias.clone(),
                    30,
                    RData::CNAME(CNAME(canonical.clone())),
                ));
            } else {
                assert_eq!(query.name(), &canonical);
                response.add_answer(Record::from_rdata(
                    canonical.clone(),
                    20,
                    RData::A(A(Ipv4Addr::new(127, 0, 0, 12))),
                ));
            }
            response
        });
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver = NativeResolver::new(
            ResolverConfig::for_test(cname_fixture.address),
            owner.waker(),
        )
        .expect("CNAME resolver must construct");
        resolver
            .resolve(ResolveKey(60), "alias-only.test".to_owned())
            .expect("CNAME resolution must submit");
        assert_eq!(
            wait_for_resolution(&mut owner, &resolver)
                .result
                .expect("CNAME resolution must succeed")
                .addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 12))]
        );
        resolver.shutdown().expect("CNAME resolver must stop");

        let ipv6_fixture = ScriptedDnsFixture::new(2, move |request| {
            let query = request.query().expect("IPv6 query must exist").clone();
            let mut response = Message::new();
            response
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .add_query(query.clone());
            if query.query_type() == RecordType::AAAA {
                response.add_answer(Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::AAAA(AAAA(Ipv6Addr::LOCALHOST)),
                ));
            } else {
                assert_eq!(query.query_type(), RecordType::A);
            }
            response
        });
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver = NativeResolver::new(
            ResolverConfig::for_test(ipv6_fixture.address),
            owner.waker(),
        )
        .expect("IPv6 resolver must construct");
        resolver
            .resolve(ResolveKey(61), "ipv6-only.test".to_owned())
            .expect("IPv6 resolution must submit");
        assert_eq!(
            wait_for_resolution(&mut owner, &resolver)
                .result
                .expect("IPv6 resolution must succeed")
                .addresses,
            vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]
        );
        resolver.shutdown().expect("IPv6 resolver must stop");
    }

    #[test]
    fn silent_nameserver_rotates_to_the_next_ranked_server() {
        let silent = StdUdpSocket::bind("127.0.0.1:0").expect("silent DNS server must bind");
        let silent_address = silent.local_addr().expect("silent DNS server address");
        let answering = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 31));
        let config = ResolverConfig::multiple_for_test(vec![silent_address, answering.address]);
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver = NativeResolver::new(config, owner.waker())
            .expect("multi-server resolver must construct");
        resolver
            .resolve(ResolveKey(80), "rotate.test".to_owned())
            .expect("multi-server resolution must submit");
        let deadline = Instant::now() + Duration::from_secs(5);
        let answer = loop {
            if let Some(result) = resolver
                .drain()
                .expect("resolver results must remain live")
                .into_iter()
                .next()
            {
                break result.result.expect("second DNS server must resolve");
            }
            assert!(Instant::now() < deadline, "multi-server rotation timed out");
            owner.poll(deadline).expect("owner wake poll must succeed");
        };
        assert_eq!(
            answer.addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 31))]
        );
        resolver
            .shutdown()
            .expect("multi-server resolver must join");
    }

    #[test]
    fn exhausted_same_server_generation_is_replaced_before_the_next_lookup() {
        let fixture =
            SourceGenerationDnsFixture::answering_new_source_ports(Ipv4Addr::new(127, 0, 0, 44));
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver =
            NativeResolver::new(ResolverConfig::for_test(fixture.address), owner.waker())
                .expect("generation-recovery resolver must construct");

        resolver
            .resolve(ResolveKey(90), "generation-prime.test".to_owned())
            .expect("prime resolution must submit");
        assert_eq!(
            wait_for_resolution(&mut owner, &resolver)
                .result
                .expect("prime resolution must prove the initial socket")
                .addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 44))]
        );

        resolver
            .resolve(ResolveKey(91), "generation-exhaust.test".to_owned())
            .expect("silent-generation resolution must submit");
        let failure = wait_for_resolution(&mut owner, &resolver)
            .result
            .expect_err("the silently poisoned generation must exhaust once");
        assert!(
            failure.message.contains("did not answer"),
            "unexpected silent-generation failure: {}",
            failure.message
        );

        resolver
            .resolve(ResolveKey(92), "generation-recovery.test".to_owned())
            .expect("post-exhaustion resolution must submit");
        assert_eq!(
            wait_for_resolution(&mut owner, &resolver)
                .result
                .expect("the next lookup must use a replacement socket generation")
                .addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 44))]
        );
        let observations = fixture.observations();
        let prime_peer = observations
            .iter()
            .find_map(|(name, peer)| (name == &fqdn("generation-prime.test")).then_some(*peer))
            .expect("fixture must observe the prime generation");
        let recovery_peer = observations
            .iter()
            .find_map(|(name, peer)| (name == &fqdn("generation-recovery.test")).then_some(*peer))
            .expect("fixture must observe the replacement generation");
        assert_ne!(
            prime_peer, recovery_peer,
            "recovery must use a new UDP source generation"
        );
        let exhausted = observations
            .iter()
            .filter(|(name, _)| name == &fqdn("generation-exhaust.test"))
            .collect::<Vec<_>>();
        assert!(
            !exhausted.is_empty(),
            "fixture must observe the exhausted lookup on the original generation"
        );
        assert!(
            exhausted.iter().all(|(_, peer)| *peer == prime_peer),
            "the already-failed lookup must not replay on the replacement generation"
        );
        resolver
            .shutdown()
            .expect("generation-recovery resolver must join");
    }

    #[test]
    fn public_resolution_uses_the_replacement_generation() {
        let fixture =
            SourceGenerationDnsFixture::answering_new_source_ports(Ipv4Addr::new(127, 0, 0, 46));
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver =
            NativeResolver::new(ResolverConfig::for_test(fixture.address), owner.waker())
                .expect("public generation-recovery resolver must construct");
        let public_spec = |host: &str| PublicResolveSpec {
            host: host.to_owned(),
            family: AddressFamily::Ipv4,
            order: AddressOrder::Ipv4ThenIpv6,
            cache_mode: CacheMode::Bypass,
            max_results: 8,
            expand_search: false,
        };

        resolver
            .public_resolve(ResolveKey(93), public_spec("public-generation-prime.test"))
            .expect("public prime resolution must submit");
        assert!(matches!(
            wait_for_resolution(&mut owner, &resolver).public,
            Some(PublicLookupOutcome::Completed {
                status: ResolveStatus::Answer,
                ref addresses,
                ..
            }) if addresses == &[IpAddr::V4(Ipv4Addr::new(127, 0, 0, 46))]
        ));

        resolver
            .public_resolve(
                ResolveKey(94),
                public_spec("public-generation-exhaust.test"),
            )
            .expect("public silent-generation resolution must submit");
        assert!(matches!(
            wait_for_resolution(&mut owner, &resolver).public,
            Some(PublicLookupOutcome::Failed(ref error))
                if error.dns_failure() == Some(DnsFailure::NoNameserver)
        ));

        resolver
            .public_resolve(
                ResolveKey(95),
                public_spec("public-generation-recovery.test"),
            )
            .expect("public post-exhaustion resolution must submit");
        assert!(matches!(
            wait_for_resolution(&mut owner, &resolver).public,
            Some(PublicLookupOutcome::Completed {
                status: ResolveStatus::Answer,
                ref addresses,
                ..
            }) if addresses == &[IpAddr::V4(Ipv4Addr::new(127, 0, 0, 46))]
        ));
        resolver
            .shutdown()
            .expect("public generation-recovery resolver must join");
    }

    #[test]
    fn concurrent_exhaustion_has_one_terminal_per_lookup_and_later_work_recovers() {
        const CONCURRENT: u64 = 16;
        let fixture =
            SourceGenerationDnsFixture::answering_new_source_ports(Ipv4Addr::new(127, 0, 0, 45));
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver =
            NativeResolver::new(ResolverConfig::for_test(fixture.address), owner.waker())
                .expect("concurrent recovery resolver must construct");

        resolver
            .resolve(ResolveKey(100), "concurrent-prime.test".to_owned())
            .expect("concurrent prime must submit");
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("concurrent prime must prove the initial generation");

        for index in 0..CONCURRENT {
            resolver
                .resolve(
                    ResolveKey(101 + index),
                    format!("concurrent-exhaust-{index}.test"),
                )
                .expect("concurrent silent lookup must submit");
        }
        let failed = wait_for_resolutions(&mut owner, &resolver, CONCURRENT as usize);
        assert!(failed.iter().all(|result| result.result.is_err()));
        let terminal_keys = failed
            .iter()
            .map(|result| result.key)
            .collect::<HashSet<_>>();
        assert_eq!(terminal_keys.len(), CONCURRENT as usize);

        resolver
            .resolve(ResolveKey(200), "concurrent-recovery.test".to_owned())
            .expect("concurrent post-exhaustion lookup must submit");
        assert_eq!(
            wait_for_resolution(&mut owner, &resolver)
                .result
                .expect("later lookup must use the replacement generation")
                .addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 45))]
        );
        resolver
            .shutdown()
            .expect("concurrent recovery resolver must join");
    }

    #[test]
    fn positive_and_authoritative_negative_results_are_cached() {
        let positive = ScriptedDnsFixture::new(1, |request| {
            let query = request.query().expect("positive cache query").clone();
            let mut response = Message::new();
            response
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .add_query(query.clone())
                .add_answer(Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::A(A(Ipv4Addr::new(127, 0, 0, 41))),
                ));
            response
        });
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver =
            NativeResolver::new(ResolverConfig::for_test(positive.address), owner.waker())
                .expect("positive-cache resolver must construct");
        for key in [ResolveKey(90), ResolveKey(91)] {
            resolver
                .resolve(key, "positive-cache.test".to_owned())
                .expect("positive-cache resolution must submit");
            assert_eq!(
                wait_for_resolution(&mut owner, &resolver)
                    .result
                    .expect("positive-cache resolution must succeed")
                    .addresses,
                vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 41))]
            );
        }
        resolver
            .shutdown()
            .expect("positive-cache resolver must join");

        let negative = ScriptedDnsFixture::new(1, |request| {
            let query = request.query().expect("negative cache query").clone();
            let zone = fqdn("test");
            let mut response = Message::new();
            response
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .set_response_code(ResponseCode::NXDomain)
                .add_query(query)
                .add_name_server(Record::from_rdata(
                    zone,
                    60,
                    RData::SOA(SOA::new(
                        fqdn("ns.test"),
                        fqdn("hostmaster.test"),
                        1,
                        60,
                        60,
                        600,
                        30,
                    )),
                ));
            response
        });
        let mut resolver =
            NativeResolver::new(ResolverConfig::for_test(negative.address), owner.waker())
                .expect("negative-cache resolver must construct");
        for key in [ResolveKey(92), ResolveKey(93)] {
            resolver
                .resolve(key, "negative-cache.test".to_owned())
                .expect("negative-cache resolution must submit");
            let failure = wait_for_resolution(&mut owner, &resolver)
                .result
                .expect_err("negative-cache resolution must fail");
            assert!(failure.message.contains("NXDomain"));
        }
        resolver
            .shutdown()
            .expect("negative-cache resolver must join");
    }

    #[test]
    fn cache_is_bounded_skips_zero_ttl_and_clamps_lifetime() {
        let now = Instant::now();
        let answer = ResolveAnswer {
            addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ttl: Duration::from_secs(60),
        };
        let mut cache = DnsCache::new();
        cache.insert(
            fqdn("zero.test"),
            Ok(answer.clone()),
            Duration::ZERO,
            MAX_POSITIVE_CACHE_TTL,
            now,
        );
        assert!(cache.entries.is_empty());
        let expiring = fqdn("expiring.test");
        cache.insert(
            expiring.clone(),
            Ok(answer.clone()),
            Duration::from_secs(1),
            MAX_POSITIVE_CACHE_TTL,
            now,
        );
        assert!(
            cache
                .get(
                    &expiring,
                    now.checked_add(Duration::from_secs(2))
                        .expect("fixture future"),
                )
                .is_none()
        );
        for index in 0..=DNS_CACHE_CAPACITY {
            cache.insert(
                fqdn(&format!("entry-{index}.test")),
                Ok(answer.clone()),
                MAX_POSITIVE_CACHE_TTL + Duration::from_secs(1),
                MAX_POSITIVE_CACHE_TTL,
                now,
            );
        }
        assert_eq!(cache.entries.len(), DNS_CACHE_CAPACITY);
        assert!(!cache.entries.contains_key(&fqdn("entry-0.test")));
        assert!(cache.entries.values().all(|entry| {
            entry.expires
                <= now
                    .checked_add(MAX_POSITIVE_CACHE_TTL)
                    .expect("fixture expiry")
        }));
    }

    #[test]
    fn resolver_uses_owned_socket_wakes_owner_and_joins() {
        let fixture = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 7));
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver =
            NativeResolver::new(ResolverConfig::for_test(fixture.address), owner.waker())
                .expect("resolver must construct");
        resolver
            .resolve(ResolveKey(7), "example.test".to_owned())
            .expect("resolution must submit");
        let deadline = Instant::now() + Duration::from_secs(1);
        let result = loop {
            let results = resolver.drain().expect("resolver results must remain live");
            if let Some(result) = results.into_iter().next() {
                break result;
            }
            assert!(Instant::now() < deadline, "resolver result timed out");
            owner.poll(deadline).expect("owner wake poll must succeed");
        };
        assert_eq!(result.key, ResolveKey(7));
        assert_eq!(
            result.result.expect("resolution must succeed").addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 7))]
        );
        let started = Instant::now();
        resolver.shutdown().expect("resolver must shut down");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn cancelled_resolution_never_delivers_and_shutdown_is_prompt() {
        let silent = StdUdpSocket::bind("127.0.0.1:0").expect("silent DNS socket must bind");
        let address = silent.local_addr().expect("silent DNS address");
        let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
        let mut resolver = NativeResolver::new(ResolverConfig::for_test(address), owner.waker())
            .expect("resolver must construct");
        resolver
            .resolve(ResolveKey(11), "cancel.test".to_owned())
            .expect("resolution must submit");
        resolver
            .cancel(ResolveKey(11))
            .expect("resolution must cancel");
        owner
            .poll(Instant::now() + Duration::from_millis(150))
            .expect("owner poll must succeed");
        assert!(
            resolver
                .drain()
                .expect("resolver results must remain live")
                .is_empty()
        );
        let started = Instant::now();
        resolver.shutdown().expect("resolver must shut down");
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(silent);
    }

    #[test]
    fn idle_resolver_safety_poll_recovers_from_a_lost_shutdown_wake() {
        let silent = StdUdpSocket::bind("127.0.0.1:0").expect("silent DNS socket must bind");
        let address = silent.local_addr().expect("silent DNS address");
        let owner = NativeReactor::new(4).expect("owner reactor must construct");
        let (polling_tx, polling_rx) = test_channel::channel();
        let mut resolver = NativeResolver::new_inner(
            ResolverConfig::for_test(address),
            owner.waker(),
            Some(polling_tx),
        )
        .expect("lost-wake resolver must construct");
        polling_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("resolver must enter its idle poll");

        let started = Instant::now();
        resolver
            .commands
            .send(Command::Shutdown)
            .expect("shutdown command must enqueue without waking");
        resolver
            .joined
            .take()
            .expect("resolver thread must be owned")
            .join()
            .expect("resolver thread must survive a lost wake");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "lost resolver wake exceeded the safety gate"
        );
    }

    #[test]
    fn hostname_resolution_feeds_the_existing_http_owner() {
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let address = listener.local_addr().expect("HTTP fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("HTTP fixture read timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("HTTP request must read");
                assert_ne!(read, 0, "client closed before HTTP request head");
                request.extend_from_slice(&buffer[..read]);
            }
            let expected_host = format!("Host: resolved.test:{}\r\n", address.port());
            assert!(
                request
                    .windows(expected_host.len())
                    .any(|window| window == expected_host.as_bytes()),
                "resolved connection must retain the URL authority"
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("HTTP response must write");
        });
        let engine = crate::testing::native_http_engine_with_nameserver(
            EngineConfig::spawned(),
            dns.address,
        )
        .expect("native DNS/HTTP Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::get(format!("http://resolved.test:{}/proof", address.port()))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("resolved HTTP request must build"),
            )
            .expect("resolved HTTP request must complete");
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"ok");
        engine.shutdown().expect("native DNS/HTTP Engine must stop");
        server.join().expect("HTTP fixture must join");
    }

    #[test]
    fn public_cancel_during_dns_is_terminal_and_engine_joins_resolver() {
        let nameserver = StdUdpSocket::bind("127.0.0.1:0").expect("DNS barrier must bind");
        nameserver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("DNS barrier timeout must configure");
        let address = nameserver.local_addr().expect("DNS barrier address");
        let engine =
            crate::testing::native_http_engine_with_nameserver(EngineConfig::spawned(), address)
                .expect("native DNS Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get("http://cancel-dns.test/proof")
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("DNS cancellation request must build"),
            )
            .expect("DNS cancellation request must submit");
        let mut query = [0_u8; DNS_PACKET_LIMIT];
        let received = nameserver
            .recv(&mut query)
            .expect("DNS barrier must observe a query");
        assert!(
            Message::from_vec(&query[..received]).is_ok(),
            "DNS barrier must observe a valid query"
        );
        pending.handle().cancel().expect("DNS request must cancel");
        assert!(matches!(pending.wait(), Completion::Cancelled));
        let started = Instant::now();
        engine.shutdown().expect("DNS Engine must shut down");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn public_stream_cancel_during_dns_is_terminal_and_engine_joins_resolver() {
        let nameserver = StdUdpSocket::bind("127.0.0.1:0").expect("stream DNS barrier must bind");
        nameserver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("stream DNS barrier timeout must configure");
        let address = nameserver.local_addr().expect("stream DNS barrier address");
        let engine =
            crate::testing::native_http_engine_with_nameserver(EngineConfig::spawned(), address)
                .expect("native stream DNS Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get("http://cancel-stream-dns.test/proof")
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("stream DNS cancellation request must build"),
            )
            .expect("stream DNS cancellation request must submit");
        let mut query = [0_u8; DNS_PACKET_LIMIT];
        let received = nameserver
            .recv(&mut query)
            .expect("stream DNS barrier must observe a query");
        assert!(Message::from_vec(&query[..received]).is_ok());
        reader
            .handle()
            .cancel()
            .expect("stream DNS request must cancel");
        assert!(matches!(
            reader.try_head(),
            Err(crate::StreamError::Cancelled)
        ));
        let started = Instant::now();
        engine.shutdown().expect("stream DNS Engine must shut down");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn resolver_pressure_cancels_live_queries_without_starving_healthy_peers() {
        const BATCH: usize = 64;
        const CANCELLED: usize = BATCH / 4;

        let nameserver =
            StdUdpSocket::bind("127.0.0.1:0").expect("resolver pressure nameserver must bind");
        nameserver
            .set_read_timeout(Some(Duration::from_millis(25)))
            .expect("resolver pressure nameserver timeout");
        let nameserver_address = nameserver
            .local_addr()
            .expect("resolver pressure nameserver address");
        let (silent_tx, silent_rx) = test_channel::channel();
        let (stop_tx, stop_rx) = test_channel::channel();
        let dns_thread = thread::spawn(move || {
            let mut buffer = [0_u8; DNS_PACKET_LIMIT];
            let mut signalled = std::collections::HashSet::new();
            loop {
                if stop_rx.try_recv().is_ok() {
                    return signalled.len();
                }
                let (length, peer) = match nameserver.recv_from(&mut buffer) {
                    Ok(received) => received,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => panic!("resolver pressure DNS receive failed: {error}"),
                };
                let request = Message::from_vec(&buffer[..length])
                    .expect("resolver pressure query must parse");
                let query = request
                    .query()
                    .expect("resolver pressure query must exist")
                    .clone();
                let name = query.name().to_utf8();
                let index = name.strip_prefix("pressure-").and_then(|name| {
                    name.split_once('.')
                        .and_then(|(index, _)| index.parse::<usize>().ok())
                });
                if index.is_some_and(|index| index % 4 == 0) {
                    let index = index.expect("silent pressure index must exist");
                    if signalled.insert(index) {
                        silent_tx
                            .send(index)
                            .expect("silent resolver query must signal");
                    }
                    continue;
                }
                let mut response = Message::new();
                response
                    .set_id(request.id())
                    .set_message_type(MessageType::Response)
                    .set_recursion_available(true)
                    .add_query(query.clone())
                    .add_answer(Record::from_rdata(
                        query.name().clone(),
                        60,
                        RData::A(A(Ipv4Addr::LOCALHOST)),
                    ));
                nameserver
                    .send_to(
                        &response
                            .to_vec()
                            .expect("resolver pressure reply must encode"),
                        peer,
                    )
                    .expect("resolver pressure reply must send");
            }
        });

        let listener =
            TcpListener::bind("127.0.0.1:0").expect("resolver pressure HTTP server must bind");
        let http_address = listener
            .local_addr()
            .expect("resolver pressure HTTP address");
        let http_thread = thread::spawn(move || {
            let mut handlers = Vec::with_capacity(BATCH - CANCELLED + 1);
            for _ in 0..BATCH - CANCELLED + 1 {
                let (mut stream, _) = listener
                    .accept()
                    .expect("resolver pressure HTTP server must accept");
                handlers.push(thread::spawn(move || {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .expect("resolver pressure HTTP timeout");
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 512];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let read = stream
                            .read(&mut buffer)
                            .expect("resolver pressure HTTP request must read");
                        assert_ne!(read, 0, "resolver pressure request closed before its head");
                        request.extend_from_slice(&buffer[..read]);
                    }
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .expect("resolver pressure HTTP response must write");
                }));
            }
            for handler in handlers {
                handler
                    .join()
                    .expect("resolver pressure HTTP handler must join");
            }
        });

        let config = EngineConfig::spawned()
            .with_max_connections(std::num::NonZeroUsize::new(8).expect("eight is non-zero"))
            .with_max_connections_per_origin(
                std::num::NonZeroUsize::new(1).expect("one is non-zero"),
            )
            .with_max_idle_connections(0)
            .with_max_idle_connections_per_origin(0);
        let engine = crate::testing::native_http_engine_with_nameserver(config, nameserver_address)
            .expect("resolver pressure Engine must construct");
        let client = engine.client();
        let mut pending = Vec::with_capacity(BATCH);
        let mut handles = Vec::with_capacity(BATCH);
        for index in 0..BATCH {
            let request = client
                .submit(
                    Request::get(format!(
                        "http://pressure-{index}.test:{}/request",
                        http_address.port()
                    ))
                    .total_timeout(Duration::from_secs(5))
                    .build()
                    .expect("resolver pressure request must build"),
                )
                .expect("resolver pressure request must submit");
            handles.push(request.handle());
            pending.push(request);
        }
        for _ in 0..CANCELLED {
            let index = silent_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("every silent DNS request must reach the nameserver");
            handles[index]
                .cancel()
                .expect("live silent DNS request must cancel");
        }

        let mut completed = 0;
        let mut cancelled = 0;
        for request in pending {
            match request.wait() {
                Completion::Completed(response) => {
                    assert_eq!(response.body(), b"ok");
                    completed += 1;
                }
                Completion::Cancelled => cancelled += 1,
                Completion::Failed(error) => {
                    panic!("resolver pressure request failed unexpectedly: {error}")
                }
            }
        }
        assert_eq!(completed, BATCH - CANCELLED);
        assert_eq!(cancelled, CANCELLED);

        let health = client
            .execute(
                Request::get(format!(
                    "http://health.test:{}/after-cancel",
                    http_address.port()
                ))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("resolver pressure health request must build"),
            )
            .expect("resolver and HTTP owner must remain healthy after cancellation pressure");
        assert_eq!(health.body(), b"ok");
        let metrics = engine.metrics();
        assert_eq!(metrics.requests_accepted(), (BATCH + 1) as u64);
        assert_eq!(metrics.requests_completed(), (completed + 1) as u64);
        assert_eq!(metrics.requests_cancelled(), cancelled as u64);
        assert_eq!(metrics.requests_failed(), 0);
        assert_eq!(metrics.high_water().active_connections(), 8);
        assert!(metrics.high_water().connection_waiters() > 0);
        assert_eq!(metrics.current().active_connections(), 0);
        assert_eq!(metrics.current().connection_waiters(), 0);
        assert_eq!(metrics.connections_opened(), (BATCH + 1) as u64);
        assert_eq!(metrics.connections_closed(), (BATCH + 1) as u64);
        engine
            .shutdown()
            .expect("resolver pressure Engine must shut down");
        stop_tx
            .send(())
            .expect("resolver pressure nameserver must stop");
        assert_eq!(
            dns_thread.join().expect("resolver pressure DNS must join"),
            CANCELLED
        );
        http_thread
            .join()
            .expect("resolver pressure HTTP server must join");
    }

    #[test]
    fn resolved_https_verifies_hostname_and_preserves_explicit_bypass() {
        let key = KeyPair::generate().expect("HTTPS fixture key must generate");
        let params = CertificateParams::new(vec!["resolved.test".to_owned()])
            .expect("HTTPS fixture parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("HTTPS fixture certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("HTTPS fixture versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("HTTPS fixture identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTPS fixture must bind");
        let address = listener.local_addr().expect("HTTPS fixture address");
        let server = thread::spawn(move || {
            let mut completed = 0_u8;
            for _ in 0..3 {
                let (stream, _) = listener.accept().expect("HTTPS fixture must accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("HTTPS fixture read timeout");
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .expect("HTTPS fixture write timeout");
                let connection = ServerConnection::new(Arc::new(server_config.clone()))
                    .expect("HTTPS server state must build");
                let mut tls = StreamOwned::new(connection, stream);
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    match tls.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            request.extend_from_slice(&buffer[..read]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                                    .expect("HTTPS response must write");
                                tls.flush().expect("HTTPS response must flush");
                                completed += 1;
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
            completed
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("native HTTPS Engine must construct");
        let verified = engine
            .client()
            .execute(
                Request::get(format!("https://resolved.test:{}/", address.port()))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("verified HTTPS request must build"),
            )
            .expect("verified HTTPS request must complete");
        assert_eq!(verified.body(), b"ok");

        let wrong_host = engine.client().execute(
            Request::get(format!("https://wrong.test:{}/", address.port()))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("wrong-host HTTPS request must build"),
        );
        match wrong_host {
            Err(ExecuteError::Failed(error)) => {
                assert_eq!(error.transport_stage(), Some(TransportStage::Tls));
            }
            other => panic!("wrong-host HTTPS must fail at TLS, got {other:?}"),
        }

        let bypass = engine
            .client()
            .execute(
                Request::get(format!("https://wrong.test:{}/", address.port()))
                    .tls_verification(TlsVerification::DangerouslyDisableCertificateVerification)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("bypass HTTPS request must build"),
            )
            .expect("explicit TLS bypass must complete");
        assert_eq!(bypass.body(), b"ok");
        engine.shutdown().expect("native HTTPS Engine must stop");
        assert_eq!(server.join().expect("HTTPS fixture must join"), 2);
    }

    #[test]
    fn synchronous_certificate_verification_blocks_unrelated_owner_work() {
        const OBSERVATION: Duration = Duration::from_millis(75);

        let key = KeyPair::generate().expect("HTTPS fixture key must generate");
        let params = CertificateParams::new(vec!["slow-verify.test".to_owned()])
            .expect("HTTPS fixture parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("HTTPS fixture certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config = Arc::new(
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("HTTPS fixture versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("HTTPS fixture identity must configure"),
        );
        let tls_listener = TcpListener::bind("127.0.0.1:0").expect("HTTPS fixture must bind");
        let tls_address = tls_listener.local_addr().expect("HTTPS fixture address");
        let tls_server = thread::spawn(move || {
            let (stream, _) = tls_listener.accept().expect("HTTPS fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("HTTPS fixture read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("HTTPS fixture write timeout");
            let connection =
                ServerConnection::new(server_config).expect("HTTPS server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = tls.read(&mut buffer).expect("HTTPS request must read");
                assert_ne!(read, 0, "HTTPS request ended before its head");
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\ntls")
                        .expect("HTTPS response must write");
                    tls.flush().expect("HTTPS response must flush");
                    return;
                }
            }
        });

        let plain_listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let plain_address = plain_listener.local_addr().expect("HTTP fixture address");
        let plain_server = thread::spawn(move || {
            let (mut stream, _) = plain_listener.accept().expect("HTTP fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("HTTP fixture read timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).expect("HTTP request must read");
                assert_ne!(read, 0, "HTTP request ended before its head");
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nplain")
                        .expect("HTTP response must write");
                    return;
                }
            }
        });

        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let (entered_tx, entered_rx) = test_channel::channel();
        let (release_tx, release_rx) = test_channel::channel();
        let config = EngineConfig::spawned();
        let factory = crate::backend::native_http::NativeHttpFactory::new_with_nameserver_and_verification_gate(
            &config,
            dns.address,
            certificate_der.as_ref().to_vec(),
            entered_tx,
            release_rx,
        )
        .expect("gated native HTTPS factory must construct");
        let engine = crate::Engine::with_spawned_factory(config, Box::new(factory))
            .expect("gated native HTTPS Engine must construct");
        let client = engine.client();
        let tls_pending = client
            .submit(
                Request::get(format!("https://slow-verify.test:{}/", tls_address.port()))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("HTTPS request must build"),
            )
            .expect("HTTPS request must submit");
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("certificate verifier must enter");

        let plain_pending = client
            .submit(
                Request::get(format!("http://{plain_address}/"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("HTTP request must build"),
            )
            .expect("unrelated HTTP request must submit");
        let plain_pending = match plain_pending.wait_for(OBSERVATION) {
            crate::WaitOutcome::TimedOut(pending) => pending,
            crate::WaitOutcome::Completed(completion) => {
                panic!("unrelated owner work escaped gated verification: {completion:?}")
            }
        };

        release_tx
            .send(())
            .expect("certificate verifier must release");
        let Completion::Completed(tls_response) = tls_pending.wait() else {
            panic!("HTTPS request must complete after verifier release")
        };
        assert_eq!(tls_response.body(), b"tls");
        let Completion::Completed(plain_response) = plain_pending.wait() else {
            panic!("unrelated HTTP request must complete after verifier release")
        };
        assert_eq!(plain_response.body(), b"plain");
        engine.shutdown().expect("gated HTTPS Engine must stop");
        tls_server.join().expect("HTTPS fixture must join");
        plain_server.join().expect("HTTP fixture must join");
    }

    #[test]
    fn public_stream_reader_drains_bounded_tls_plaintext_through_dns_and_https() {
        let key = KeyPair::generate().expect("stream HTTPS key must generate");
        let params = CertificateParams::new(vec!["stream.test".to_owned()])
            .expect("stream HTTPS parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("stream HTTPS certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("stream HTTPS versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("stream HTTPS identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("stream HTTPS fixture must bind");
        let address = listener.local_addr().expect("stream HTTPS fixture address");
        let expected = vec![b't'; 64 * 1024];
        let server_body = expected.clone();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("stream HTTPS fixture must accept");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("stream HTTPS server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = tls
                    .read(&mut buffer)
                    .expect("stream HTTPS request must read");
                assert_ne!(read, 0, "client closed before stream HTTPS request");
                request.extend_from_slice(&buffer[..read]);
            }
            tls.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    server_body.len()
                )
                .as_bytes(),
            )
            .expect("stream HTTPS response head must write");
            tls.write_all(&server_body)
                .expect("stream HTTPS response body must write");
            tls.flush().expect("stream HTTPS response must flush");
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let config = EngineConfig::spawned()
            .with_max_stream_queue_bytes_per_request(257)
            .with_max_stream_queued_bytes(257);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            config,
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("stream HTTPS Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("https://stream.test:{}/", address.port()))
                    .total_timeout(Duration::from_secs(3))
                    .build()
                    .expect("stream HTTPS request must build"),
            )
            .expect("stream HTTPS request must submit");
        assert_eq!(
            reader
                .wait_head()
                .expect("stream HTTPS head must arrive")
                .status(),
            200
        );
        let mut received = Vec::new();
        let mut hole = [0_u8; 101];
        while let Some(read) = reader.read(&mut hole).expect("stream HTTPS body must read") {
            received.extend_from_slice(&hole[..read]);
        }
        assert_eq!(received, expected);
        engine.shutdown().expect("stream HTTPS Engine must stop");
        server.join().expect("stream HTTPS fixture must join");
    }

    #[test]
    fn fixed_streamed_upload_pumps_incrementally_through_rustls() {
        let key = KeyPair::generate().expect("upload HTTPS key must generate");
        let params = CertificateParams::new(vec!["upload.test".to_owned()])
            .expect("upload HTTPS parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("upload HTTPS certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("upload HTTPS versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("upload HTTPS identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("upload HTTPS fixture must bind");
        let address = listener.local_addr().expect("upload HTTPS fixture address");
        let expected = (0..64 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let server_expected = expected.clone();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("upload HTTPS fixture must accept");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("upload HTTPS server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2048];
            let head_end = loop {
                if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    break position + 4;
                }
                let read = tls.read(&mut buffer).expect("upload HTTPS head must read");
                assert_ne!(read, 0, "client closed before upload HTTPS head");
                request.extend_from_slice(&buffer[..read]);
            };
            let head = std::str::from_utf8(&request[..head_end]).expect("upload head is UTF-8");
            assert!(head.contains("Content-Length: 65536\r\n"));
            while request.len() < head_end + server_expected.len() {
                let read = tls.read(&mut buffer).expect("upload HTTPS body must read");
                assert_ne!(read, 0, "client closed before upload HTTPS body");
                request.extend_from_slice(&buffer[..read]);
            }
            assert_eq!(
                &request[head_end..head_end + server_expected.len()],
                server_expected
            );
            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("upload HTTPS response must write");
            tls.flush().expect("upload HTTPS response must flush");
        });

        let (body, mut sender) =
            UploadBody::fixed(expected.len() as u64, 4096).expect("upload pair must construct");
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4096);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            config,
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("upload HTTPS Engine must construct");
        let reader = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("https://upload.test:{}/", address.port()))
                    .body_stream(body)
                    .total_timeout(Duration::from_secs(3))
                    .build()
                    .expect("upload HTTPS request must build"),
            )
            .expect("upload HTTPS request must submit");
        sender
            .push(expected)
            .expect("blocking upload HTTPS producer must cross its small queue window");
        sender.finish().expect("upload HTTPS producer must finish");
        let response = reader
            .collect()
            .expect("upload HTTPS response must collect");
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"ok");
        engine.shutdown().expect("upload HTTPS Engine must stop");
        server.join().expect("upload HTTPS fixture must join");
    }

    #[test]
    fn public_cancel_during_tls_handshake_closes_peer_and_joins() {
        let key = KeyPair::generate().expect("TLS stall key must generate");
        let params = CertificateParams::new(vec!["stall.test".to_owned()])
            .expect("TLS stall parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("TLS stall certificate must sign");
        let listener = TcpListener::bind("127.0.0.1:0").expect("TLS stall must bind");
        let address = listener.local_addr().expect("TLS stall address");
        let (hello_tx, hello_rx) = test_channel::channel();
        let server = thread::spawn(move || {
            let mut closes = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("TLS stall must accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("TLS stall read timeout");
                let mut buffer = [0_u8; 4096];
                let read = stream
                    .read(&mut buffer)
                    .expect("TLS stall must read ClientHello");
                assert_ne!(read, 0, "TLS client closed before ClientHello");
                hello_tx.send(()).expect("TLS stall barrier must signal");
                let started = Instant::now();
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => {
                            closes.push(started.elapsed());
                            break;
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                            panic!("TLS peer was not closed after cancellation")
                        }
                        Err(_) => {
                            closes.push(started.elapsed());
                            break;
                        }
                    }
                }
            }
            closes
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate.der().as_ref().to_vec(),
        )
        .expect("TLS stall Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get(format!("https://stall.test:{}/", address.port()))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("TLS stall request must build"),
            )
            .expect("TLS stall request must submit");
        hello_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("TLS stall must observe ClientHello");
        pending
            .handle()
            .cancel()
            .expect("TLS handshake request must cancel");
        assert!(matches!(pending.wait(), Completion::Cancelled));

        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("https://stall.test:{}/stream", address.port()))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("TLS stream stall request must build"),
            )
            .expect("TLS stream stall request must submit");
        hello_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("TLS stream stall must observe ClientHello");
        reader
            .handle()
            .cancel()
            .expect("TLS stream handshake request must cancel");
        assert!(matches!(
            reader.try_head(),
            Err(crate::StreamError::Cancelled)
        ));

        let peer_closes = server.join().expect("TLS stall must join");
        assert!(
            peer_closes
                .into_iter()
                .all(|elapsed| elapsed < Duration::from_millis(500))
        );
        let started = Instant::now();
        engine.shutdown().expect("TLS stall Engine must stop");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn https_rejects_dirty_close_delimited_eof() {
        let key = KeyPair::generate().expect("dirty EOF key must generate");
        let params = CertificateParams::new(vec!["dirty.test".to_owned()])
            .expect("dirty EOF parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("dirty EOF certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("dirty EOF versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("dirty EOF identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("dirty EOF fixture must bind");
        let address = listener.local_addr().expect("dirty EOF fixture address");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("dirty EOF fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("dirty EOF read timeout");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("dirty EOF server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = tls.read(&mut buffer).expect("dirty EOF request must read");
                assert_ne!(read, 0, "client closed before dirty EOF request");
                request.extend_from_slice(&buffer[..read]);
            }
            tls.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\npartial")
                .expect("dirty EOF response must encrypt");
            tls.flush().expect("dirty EOF response must flush");
            // Dropping the transport without send_close_notify deliberately produces raw TCP EOF.
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("dirty EOF Engine must construct");
        let result = engine.client().execute(
            Request::get(format!("https://dirty.test:{}/", address.port()))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("dirty EOF request must build"),
        );
        let Err(ExecuteError::Failed(error)) = result else {
            panic!("dirty TLS EOF must fail, got {result:?}");
        };
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert_eq!(error.transport_stage(), Some(TransportStage::Receive));
        engine.shutdown().expect("dirty EOF Engine must stop");
        server.join().expect("dirty EOF fixture must join");
    }

    #[test]
    fn https_plaintext_response_limit_wins_before_buffer_growth() {
        let key = KeyPair::generate().expect("TLS limit key must generate");
        let params = CertificateParams::new(vec!["limit.test".to_owned()])
            .expect("TLS limit parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("TLS limit certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS limit versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("TLS limit identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("TLS limit fixture must bind");
        let address = listener.local_addr().expect("TLS limit fixture address");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("TLS limit fixture must accept");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("TLS limit server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = tls.read(&mut buffer).expect("TLS limit request must read");
                assert_ne!(read, 0, "client closed before TLS limit request");
                request.extend_from_slice(&buffer[..read]);
            }
            tls.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\none\r\n3\r\ntwo\r\n0\r\n\r\n",
            )
            .expect("TLS oversize response must encrypt");
            tls.flush().expect("TLS oversize response must flush");
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned().with_max_response_body_bytes(4),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("TLS limit Engine must construct");
        let result = engine.client().execute(
            Request::get(format!("https://limit.test:{}/", address.port()))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("TLS limit request must build"),
        );
        let Err(ExecuteError::Failed(error)) = result else {
            panic!("oversize TLS plaintext must fail, got {result:?}");
        };
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
        engine.shutdown().expect("TLS limit Engine must stop");
        server.join().expect("TLS limit fixture must join");
    }

    #[test]
    fn manual_native_https_drives_dns_tls_and_http_to_completion() {
        let key = KeyPair::generate().expect("manual HTTPS key must generate");
        let params = CertificateParams::new(vec!["manual.test".to_owned()])
            .expect("manual HTTPS parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("manual HTTPS certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("manual HTTPS versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("manual HTTPS identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("manual HTTPS fixture must bind");
        let address = listener.local_addr().expect("manual HTTPS fixture address");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("manual HTTPS fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("manual HTTPS read timeout");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("manual HTTPS server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            for body in [b"manual".as_slice(), b"stream".as_slice()] {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = tls
                        .read(&mut buffer)
                        .expect("manual HTTPS request must read");
                    assert_ne!(read, 0, "client closed before manual HTTPS request");
                    request.extend_from_slice(&buffer[..read]);
                }
                tls.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .expect("manual HTTPS response head must write");
                tls.write_all(body)
                    .expect("manual HTTPS response body must write");
                tls.flush().expect("manual HTTPS response must flush");
            }
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let mut engine = crate::testing::native_https_manual_engine_with_nameserver_and_test_root(
            EngineConfig::manual().with_max_stream_queue_bytes_per_request(3),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("manual native HTTPS Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get(format!("https://manual.test:{}/manual", address.port()))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("manual HTTPS request must build"),
            )
            .expect("manual HTTPS request must submit");
        let completion = engine
            .drive_until(pending)
            .expect("manual Engine must drive DNS, TLS, and HTTP");
        let Completion::Completed(response) = completion else {
            panic!("manual native HTTPS request did not complete");
        };
        assert_eq!(response.body(), b"manual");

        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("https://manual.test:{}/stream", address.port()))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("manual HTTPS stream request must build"),
            )
            .expect("manual HTTPS stream request must submit");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut streamed = Vec::new();
        let mut buffer = [0_u8; 2];
        loop {
            engine
                .drive((Instant::now() + Duration::from_millis(10)).min(deadline))
                .expect("manual HTTPS stream drive must succeed");
            match reader
                .try_read(&mut buffer)
                .expect("manual HTTPS stream read must succeed")
            {
                crate::StreamRead::Pending => {
                    assert!(Instant::now() < deadline, "manual HTTPS stream timed out")
                }
                crate::StreamRead::Data(read) => streamed.extend_from_slice(&buffer[..read]),
                crate::StreamRead::Eof => break,
            }
        }
        assert_eq!(streamed, b"stream");
        engine
            .shutdown()
            .expect("manual native HTTPS Engine must stop");
        server.join().expect("manual HTTPS fixture must join");
    }

    #[test]
    fn native_https_reuses_one_clean_tls_connection_for_sequential_requests() {
        let key = KeyPair::generate().expect("reused HTTPS key must generate");
        let params = CertificateParams::new(vec!["reuse.test".to_owned()])
            .expect("reused HTTPS parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("reused HTTPS certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("reused HTTPS versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("reused HTTPS identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("reused HTTPS fixture must bind");
        let address = listener.local_addr().expect("reused HTTPS fixture address");
        let server = thread::spawn(move || {
            let (stream, _) = listener
                .accept()
                .expect("reused HTTPS fixture must accept once");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("reused HTTPS read timeout");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("reused HTTPS server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            for body in [b"one".as_slice(), b"two".as_slice()] {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = tls
                        .read(&mut buffer)
                        .expect("reused HTTPS request must read");
                    assert_ne!(read, 0, "client closed the reusable TLS connection early");
                    request.extend_from_slice(&buffer[..read]);
                }
                tls.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .expect("reused HTTPS response head must write");
                tls.write_all(body)
                    .expect("reused HTTPS response body must write");
                tls.flush().expect("reused HTTPS response must flush");
            }
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("reused native HTTPS Engine must construct");
        for expected in [b"one".as_slice(), b"two".as_slice()] {
            let response = engine
                .client()
                .execute(
                    Request::get(format!("https://reuse.test:{}/reuse", address.port()))
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("reused HTTPS request must build"),
                )
                .expect("reused HTTPS request must complete");
            assert_eq!(response.body(), expected);
        }
        engine.shutdown().expect("reused HTTPS Engine must stop");
        server.join().expect("reused HTTPS fixture must join");
    }

    #[test]
    fn native_pool_key_isolates_host_and_tls_verification_policy() {
        let key = KeyPair::generate().expect("pool-key TLS key must generate");
        let params = CertificateParams::new(vec!["a.test".to_owned(), "b.test".to_owned()])
            .expect("pool-key TLS parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("pool-key TLS certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config = Arc::new(
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("pool-key TLS versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("pool-key TLS identity must configure"),
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("pool-key fixture must bind");
        let address = listener.local_addr().expect("pool-key fixture address");
        let server = thread::spawn(move || {
            let mut live_connections = Vec::new();
            for body in [b"verified-a".as_slice(), b"bypass-a", b"verified-b"] {
                let (stream, _) = listener
                    .accept()
                    .expect("each distinct pool key must open a socket");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("pool-key TLS read timeout");
                let connection = ServerConnection::new(Arc::clone(&server_config))
                    .expect("pool-key TLS server state must build");
                let mut tls = StreamOwned::new(connection, stream);
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = tls.read(&mut buffer).expect("pool-key request must read");
                    assert_ne!(read, 0, "pool-key request closed early");
                    request.extend_from_slice(&buffer[..read]);
                }
                tls.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .expect("pool-key response head must write");
                tls.write_all(body)
                    .expect("pool-key response body must write");
                tls.flush().expect("pool-key response must flush");
                live_connections.push(tls);
            }
            assert_eq!(live_connections.len(), 3);
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("pool-key native HTTPS Engine must construct");
        for (host, verification, expected) in [
            ("a.test", TlsVerification::Verify, b"verified-a".as_slice()),
            (
                "a.test",
                TlsVerification::DangerouslyDisableCertificateVerification,
                b"bypass-a".as_slice(),
            ),
            ("b.test", TlsVerification::Verify, b"verified-b".as_slice()),
        ] {
            let response = engine
                .client()
                .execute(
                    Request::get(format!("https://{host}:{}/pool-key", address.port()))
                        .tls_verification(verification)
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("pool-key request must build"),
                )
                .expect("pool-key request must complete");
            assert_eq!(response.body(), expected);
        }
        engine.shutdown().expect("pool-key Engine must stop");
        server.join().expect("pool-key fixture must join");
    }

    #[test]
    fn encrypted_record_error_after_reuse_destroys_connection_before_replacement() {
        let key = KeyPair::generate().expect("reused TLS-error key must generate");
        let params = CertificateParams::new(vec!["reuse-error.test".to_owned()])
            .expect("reused TLS-error parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("reused TLS-error certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config = Arc::new(
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("reused TLS-error versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("reused TLS-error identity must configure"),
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("reused TLS-error must bind");
        let address = listener.local_addr().expect("reused TLS-error address");
        let server = thread::spawn(move || {
            let read_head = |tls: &mut StreamOwned<ServerConnection, std::net::TcpStream>| {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = tls
                        .read(&mut buffer)
                        .expect("reused TLS-error request must read");
                    assert_ne!(read, 0, "reused TLS-error request closed early");
                    request.extend_from_slice(&buffer[..read]);
                }
            };
            let (first_stream, _) = listener
                .accept()
                .expect("reused TLS-error first socket must accept");
            first_stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("reused TLS-error first timeout");
            let first_connection = ServerConnection::new(Arc::clone(&server_config))
                .expect("reused TLS-error first state must build");
            let mut first = StreamOwned::new(first_connection, first_stream);
            read_head(&mut first);
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("reused TLS-error clean response must write");
            first
                .flush()
                .expect("reused TLS-error clean response must flush");
            read_head(&mut first);
            first
                .sock
                .write_all(&[0x17, 0x03, 0x03, 0x00, 0x01, 0x00])
                .expect("invalid TLS record must write");
            first.sock.flush().expect("invalid TLS record must flush");
            drop(first);

            let (replacement_stream, _) = listener
                .accept()
                .expect("TLS-error replacement socket must accept");
            replacement_stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("TLS-error replacement timeout");
            let replacement_connection = ServerConnection::new(server_config)
                .expect("TLS-error replacement state must build");
            let mut replacement = StreamOwned::new(replacement_connection, replacement_stream);
            read_head(&mut replacement);
            replacement
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew")
                .expect("TLS-error replacement response must write");
            replacement
                .flush()
                .expect("TLS-error replacement response must flush");
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("reused TLS-error Engine must construct");
        let request = || {
            Request::get(format!(
                "https://reuse-error.test:{}/reuse-error",
                address.port()
            ))
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("reused TLS-error request must build")
        };
        assert_eq!(
            engine
                .client()
                .execute(request())
                .expect("reused TLS-error first request must complete")
                .body(),
            b"ok"
        );
        let Err(ExecuteError::Failed(error)) = engine.client().execute(request()) else {
            panic!("invalid TLS record on reused connection did not fail");
        };
        assert_eq!(error.kind(), ErrorKind::Transport);
        // TLS handshake failures use `Tls`; corruption after an established handshake is a
        // receive-stage transport failure. Both are destructive to the lease.
        assert_eq!(error.transport_stage(), Some(TransportStage::Receive));
        assert_eq!(
            engine
                .client()
                .execute(request())
                .expect("TLS-error replacement request must complete")
                .body(),
            b"new"
        );
        engine
            .shutdown()
            .expect("reused TLS-error Engine must stop");
        server.join().expect("reused TLS-error fixture must join");
    }

    #[test]
    fn buffered_https_upload_over_tls_flight_limit_pumps_incrementally() {
        let key = KeyPair::generate().expect("large HTTPS key must generate");
        let params = CertificateParams::new(vec!["large-upload.test".to_owned()])
            .expect("large HTTPS parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("large HTTPS certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("large HTTPS versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("large HTTPS identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("large HTTPS fixture must bind");
        let address = listener.local_addr().expect("large HTTPS fixture address");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("large HTTPS fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("large HTTPS read timeout");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("large HTTPS server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            let mut request = Vec::new();
            let mut buffer = [0_u8; 16 * 1024];
            let mut expected = None;
            loop {
                match tls.read(&mut buffer) {
                    Ok(0) => panic!("large HTTPS peer closed before the request completed"),
                    Ok(read) => {
                        request.extend_from_slice(&buffer[..read]);
                        if expected.is_none() {
                            if let Some(head) = request
                                .windows(4)
                                .position(|window| window == b"\r\n\r\n")
                                .map(|position| position + 4)
                            {
                                let content_length = std::str::from_utf8(&request[..head])
                                    .expect("large HTTPS head must be UTF-8")
                                    .lines()
                                    .find_map(|line| {
                                        line.split_once(':').and_then(|(name, value)| {
                                            name.eq_ignore_ascii_case("content-length").then(|| {
                                                value
                                                    .trim()
                                                    .parse::<usize>()
                                                    .expect("large HTTPS Content-Length")
                                            })
                                        })
                                    })
                                    .expect("large HTTPS request must carry Content-Length");
                                expected = Some(head + content_length);
                            }
                        }
                        if expected.is_some_and(|expected| request.len() >= expected) {
                            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                                .expect("large HTTPS response must write");
                            tls.flush().expect("large HTTPS response must flush");
                            let first_length = request.len();
                            let mut second = Vec::new();
                            while !second.windows(4).any(|window| window == b"\r\n\r\n") {
                                let read = tls
                                    .read(&mut buffer)
                                    .expect("request after large upload must read");
                                assert_ne!(read, 0, "client closed before reused HTTPS request");
                                second.extend_from_slice(&buffer[..read]);
                            }
                            assert!(second.starts_with(b"GET /reused HTTP/1.1\r\n"));
                            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nreused")
                                .expect("reused HTTPS response must write");
                            tls.flush().expect("reused HTTPS response must flush");
                            return first_length;
                        }
                    }
                    Err(error) => panic!("large HTTPS request failed to read: {error}"),
                }
            }
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("large native HTTPS Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::post(format!("https://large-upload.test:{}/", address.port()))
                    .body(vec![b'x'; TLS_FLIGHT_LIMIT + 1])
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("large HTTPS request must build"),
            )
            .expect("large HTTPS upload must complete through the incremental pump");
        assert_eq!(response.body(), b"ok");
        let reused = engine
            .client()
            .execute(
                Request::get(format!(
                    "https://large-upload.test:{}/reused",
                    address.port()
                ))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("request after large HTTPS upload must build"),
            )
            .expect("clean large upload connection must be reusable");
        assert_eq!(reused.body(), b"reused");
        engine.shutdown().expect("large HTTPS Engine must stop");
        assert!(
            server.join().expect("large HTTPS fixture must join") > TLS_FLIGHT_LIMIT,
            "server must receive more plaintext than the bounded ciphertext queue"
        );
    }

    #[test]
    fn cancellation_during_incremental_https_upload_closes_promptly() {
        let key = KeyPair::generate().expect("cancel-upload HTTPS key must generate");
        let params = CertificateParams::new(vec!["cancel-upload.test".to_owned()])
            .expect("cancel-upload HTTPS parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("cancel-upload HTTPS certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("cancel-upload HTTPS versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("cancel-upload HTTPS identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("cancel-upload fixture must bind");
        let address = listener
            .local_addr()
            .expect("cancel-upload fixture address");
        let (progress_tx, progress_rx) = test_channel::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener
                .accept()
                .expect("cancel-upload fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("cancel-upload read timeout");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("cancel-upload HTTPS server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            let mut received = 0_usize;
            let mut buffer = [0_u8; 16 * 1024];
            while received < 128 * 1024 {
                let read = tls
                    .read(&mut buffer)
                    .expect("cancel-upload prefix must read");
                assert_ne!(read, 0, "client closed before incremental upload progress");
                received += read;
            }
            progress_tx
                .send(())
                .expect("cancel-upload barrier must signal");
            let started = Instant::now();
            loop {
                match tls.read(&mut buffer) {
                    Ok(0) | Err(_) => return started.elapsed(),
                    Ok(_) => {}
                }
            }
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("cancel-upload native HTTPS Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::post(format!("https://cancel-upload.test:{}/", address.port()))
                    .body(vec![b'x'; 8 * 1024 * 1024])
                    .total_timeout(Duration::from_secs(5))
                    .build()
                    .expect("cancel-upload HTTPS request must build"),
            )
            .expect("cancel-upload HTTPS request must submit");
        progress_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server must observe incremental upload progress");
        pending
            .handle()
            .cancel()
            .expect("incremental upload must cancel");
        assert!(matches!(pending.wait(), Completion::Cancelled));
        engine.shutdown().expect("cancel-upload Engine must stop");
        let close_latency = server.join().expect("cancel-upload fixture must join");
        assert!(
            close_latency < Duration::from_millis(500),
            "incremental upload socket close took {close_latency:?}"
        );
    }

    #[test]
    fn early_https_response_does_not_overfill_incremental_upload_queue() {
        let key = KeyPair::generate().expect("early-response HTTPS key must generate");
        let params = CertificateParams::new(vec!["early-response.test".to_owned()])
            .expect("early-response HTTPS parameters must build");
        let certificate = params
            .self_signed(&key)
            .expect("early-response HTTPS certificate must sign");
        let certificate_der = certificate.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("early-response HTTPS versions must configure")
                .with_no_client_auth()
                .with_single_cert(vec![certificate_der.clone()], private_key)
                .expect("early-response HTTPS identity must configure");
        let listener = TcpListener::bind("127.0.0.1:0").expect("early-response fixture must bind");
        let address = listener
            .local_addr()
            .expect("early-response fixture address");
        let server = thread::spawn(move || {
            let (stream, _) = listener
                .accept()
                .expect("early-response fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("early-response read timeout");
            let connection = ServerConnection::new(Arc::new(server_config))
                .expect("early-response HTTPS server state must build");
            let mut tls = StreamOwned::new(connection, stream);
            let mut received = 0_usize;
            let mut buffer = [0_u8; 16 * 1024];
            while received < 128 * 1024 {
                let read = tls
                    .read(&mut buffer)
                    .expect("early-response upload prefix must read");
                assert_ne!(read, 0, "client closed before early response threshold");
                received += read;
            }
            tls.write_all(
                b"HTTP/1.1 413 Payload Too Large\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            )
            .expect("early HTTPS response must write");
            tls.flush().expect("early HTTPS response must flush");
            loop {
                match tls.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        let dns = DnsFixture::answering(Ipv4Addr::LOCALHOST);
        let engine = crate::testing::native_https_engine_with_nameserver_and_test_root(
            EngineConfig::spawned(),
            dns.address,
            certificate_der.as_ref().to_vec(),
        )
        .expect("early-response native HTTPS Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::post(format!("https://early-response.test:{}/", address.port()))
                    .body(vec![b'x'; 8 * 1024 * 1024])
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("early-response HTTPS request must build"),
            )
            .expect("early HTTP status must remain a completed response");
        assert_eq!(response.status(), 413);
        engine.shutdown().expect("early-response Engine must stop");
        server.join().expect("early-response fixture must join");
    }
}
