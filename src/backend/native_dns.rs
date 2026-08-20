#![cfg_attr(not(test), allow(dead_code))]

//! Private Engine-owned nonblocking DNS service.
//!
//! Hickory is used only for DNS wire encoding and decoding. NBReq owns the socket, poll loop,
//! retry clock, command/result queues, cancellation, wakeup, and joined shutdown.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, MessageType, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use mio::net::{TcpStream, UdpSocket};
use mio::{Events, Interest, Poll, Token, Waker};

use super::native::NativeWaker;
use crate::{Error, ErrorKind};

const WAKE_TOKEN: Token = Token(0);
const SOCKET_TOKEN: Token = Token(1);
const FIRST_TCP_TOKEN: usize = 2;
const DNS_PACKET_LIMIT: usize = 4096;
const DEFAULT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_ATTEMPTS: u8 = 3;
const MAX_CNAME_HOPS: u8 = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct ResolveKey(pub(super) u64);

#[derive(Clone, Debug)]
pub(super) struct ResolverConfig {
    pub(super) nameservers: Vec<SocketAddr>,
    pub(super) attempt_timeout: Duration,
    pub(super) attempts: u8,
}

impl ResolverConfig {
    pub(super) fn injected(nameserver: SocketAddr) -> Self {
        Self {
            nameservers: vec![nameserver],
            attempt_timeout: DEFAULT_ATTEMPT_TIMEOUT,
            attempts: DEFAULT_ATTEMPTS,
        }
    }

    pub(super) fn system() -> Result<Self, Error> {
        let discovered = super::native_dns_config::discover()?;
        Ok(Self {
            nameservers: discovered.nameservers,
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
            attempt_timeout: Duration::from_millis(20),
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
}

impl ResolveFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub(super) struct ResolveResult {
    pub(super) key: ResolveKey,
    pub(super) result: Result<ResolveAnswer, ResolveFailure>,
}

enum Command {
    Resolve { key: ResolveKey, host: String },
    Cancel(ResolveKey),
    Shutdown,
}

struct PendingQuery {
    key: ResolveKey,
    host: Name,
    record_type: RecordType,
    cname_hops: u8,
    wire: Vec<u8>,
    attempts_sent: u8,
    servers_tried: usize,
    next_attempt: Instant,
    transport: QueryTransport,
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
}

pub(super) struct NativeResolver {
    commands: Sender<Command>,
    results: Receiver<ResolveResult>,
    waker: Arc<Waker>,
    joined: Option<JoinHandle<()>>,
}

impl NativeResolver {
    pub(super) fn new(config: ResolverConfig, result_waker: NativeWaker) -> Result<Self, Error> {
        if config.attempts == 0 || config.attempt_timeout.is_zero() {
            return Err(Error::new(
                ErrorKind::Internal,
                "native resolver retry configuration must be nonzero",
            ));
        }
        let mut poll = Poll::new().map_err(|error| resolver_internal("poll creation", &error))?;
        let waker = Arc::new(
            Waker::new(poll.registry(), WAKE_TOKEN)
                .map_err(|error| resolver_internal("waker creation", &error))?,
        );
        let (mut socket, current_nameserver) = connect_nameserver(&config.nameservers)?;
        poll.registry()
            .register(&mut socket, SOCKET_TOKEN, Interest::READABLE)
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
    poll: &mut Poll,
    socket: &mut UdpSocket,
    commands: Receiver<Command>,
    results: Sender<ResolveResult>,
    result_waker: NativeWaker,
    config: ResolverConfig,
    mut state: ResolverState,
) {
    let mut events = Events::with_capacity(16);
    loop {
        let mut stop = false;
        loop {
            match commands.try_recv() {
                Ok(Command::Resolve { key, host }) => {
                    if let Some(previous) = state.by_key.remove(&key) {
                        remove_query(previous, &mut state, poll);
                    }
                    match prepare_query(
                        key,
                        &host,
                        RecordType::A,
                        0,
                        &state.pending,
                        &mut state.next_id,
                    ) {
                        Ok((id, query)) => {
                            state.by_key.insert(key, id);
                            state.pending.insert(id, query);
                        }
                        Err(failure) => send_result(
                            &results,
                            &result_waker,
                            ResolveResult {
                                key,
                                result: Err(failure),
                            },
                        ),
                    }
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
            .min();
        if poll.poll(&mut events, timeout).is_err() {
            fail_all(
                &mut state.pending,
                &mut state.by_key,
                &results,
                &result_waker,
                "native resolver poll failed",
            );
            return;
        }
        let socket_ready = events
            .iter()
            .any(|event| event.token() == SOCKET_TOKEN && event.is_readable());
        if socket_ready {
            receive_packets(socket, &mut state, &results, &result_waker, poll, &config);
        }
        let tcp_events = events
            .iter()
            .filter(|event| event.token().0 >= FIRST_TCP_TOKEN)
            .map(|event| (event.token(), event.is_readable(), event.is_writable()))
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

fn prepare_query(
    key: ResolveKey,
    host: &str,
    record_type: RecordType,
    cname_hops: u8,
    pending: &HashMap<u16, PendingQuery>,
    next_id: &mut u16,
) -> Result<(u16, PendingQuery), ResolveFailure> {
    let mut name =
        Name::from_ascii(host).map_err(|_| ResolveFailure::new("the DNS hostname is invalid"))?;
    name.set_fqdn(true);
    prepare_name_query(key, name, record_type, cname_hops, pending, next_id)
}

fn prepare_name_query(
    key: ResolveKey,
    name: Name,
    record_type: RecordType,
    cname_hops: u8,
    pending: &HashMap<u16, PendingQuery>,
    next_id: &mut u16,
) -> Result<(u16, PendingQuery), ResolveFailure> {
    let id = allocate_id(pending, next_id)
        .ok_or_else(|| ResolveFailure::new("the native resolver transaction space is exhausted"))?;
    let mut message = Message::new();
    message
        .set_id(id)
        .set_recursion_desired(true)
        .add_query(Query::query(name.clone(), record_type));
    let wire = message
        .to_vec()
        .map_err(|_| ResolveFailure::new("the DNS query could not be encoded"))?;
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
            next_attempt: Instant::now(),
            transport: QueryTransport::Udp,
        },
    ))
}

fn allocate_id(pending: &HashMap<u16, PendingQuery>, next_id: &mut u16) -> Option<u16> {
    for _ in 0..=u16::MAX {
        let candidate = *next_id;
        *next_id = next_id.wrapping_add(1);
        if !pending.contains_key(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn transmit_due(
    socket: &mut UdpSocket,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
    config: &ResolverConfig,
    poll: &Poll,
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
                    ResolveResult {
                        key: query.key,
                        result: Err(ResolveFailure::new(
                            "the DNS-over-TCP fallback did not complete before its deadline",
                        )),
                    },
                );
            }
            continue;
        }
        let exhausted = state.pending.get(&id).is_some_and(|query| {
            matches!(query.transport, QueryTransport::Udp) && query.attempts_sent >= config.attempts
        });
        if exhausted {
            let may_try_another = state
                .pending
                .get(&id)
                .is_some_and(|query| query.servers_tried < config.nameservers.len());
            if may_try_another
                && advance_nameserver(socket, poll, config, &mut state.current_nameserver)
            {
                for query in state.pending.values_mut() {
                    if matches!(query.transport, QueryTransport::Udp) {
                        query.attempts_sent = 0;
                        query.servers_tried = query.servers_tried.saturating_add(1);
                        query.next_attempt = now;
                    }
                }
            } else {
                if let Some(query) = state.pending.remove(&id) {
                    state.by_key.remove(&query.key);
                    send_result(
                        results,
                        result_waker,
                        ResolveResult {
                            key: query.key,
                            result: Err(ResolveFailure::new(
                                "the configured DNS servers did not answer within the retry budget",
                            )),
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
                query.next_attempt = now.checked_add(config.attempt_timeout).unwrap_or(now);
            }
            Ok(_) => {
                query.attempts_sent = config.attempts;
                query.next_attempt = now;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                query.next_attempt = now.checked_add(Duration::from_millis(1)).unwrap_or(now);
            }
            Err(_) => {
                query.attempts_sent = config.attempts;
                query.next_attempt = now;
            }
        }
    }
}

fn advance_nameserver(
    socket: &mut UdpSocket,
    poll: &Poll,
    config: &ResolverConfig,
    current: &mut usize,
) -> bool {
    for offset in 1..config.nameservers.len() {
        let index = (*current + offset) % config.nameservers.len();
        let Ok(mut replacement) = connect_one_nameserver(config.nameservers[index]) else {
            continue;
        };
        if poll
            .registry()
            .register(&mut replacement, SOCKET_TOKEN, Interest::READABLE)
            .is_err()
        {
            continue;
        }
        let _deregister_result = poll.registry().deregister(socket);
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
    poll: &Poll,
    config: &ResolverConfig,
) {
    let mut buffer = [0_u8; DNS_PACKET_LIMIT];
    loop {
        let length = match socket.recv(&mut buffer) {
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => return,
        };
        if length < 2 {
            continue;
        }
        let id = u16::from_be_bytes([buffer[0], buffer[1]]);
        let Some(query) = state.pending.get(&id) else {
            continue;
        };
        let result = parse_answer(&buffer[..length], id, &query.host, query.record_type);
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
                    send_result(
                        results,
                        result_waker,
                        ResolveResult {
                            key: query.key,
                            result: Err(failure),
                        },
                    );
                }
            }
            continue;
        }
        finish_answer(query, result, state, results, result_waker);
    }
}

fn finish_answer(
    query: PendingQuery,
    result: Result<ParsedAnswer, ResolveFailure>,
    state: &mut ResolverState,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
) {
    match result {
        Ok(ParsedAnswer::Answer(answer)) => {
            state.by_key.remove(&query.key);
            send_result(
                results,
                result_waker,
                ResolveResult {
                    key: query.key,
                    result: Ok(answer),
                },
            );
        }
        Ok(ParsedAnswer::Canonical(canonical)) if query.cname_hops < MAX_CNAME_HOPS => {
            match prepare_name_query(
                query.key,
                canonical,
                query.record_type,
                query.cname_hops + 1,
                &state.pending,
                &mut state.next_id,
            ) {
                Ok((next, replacement)) => {
                    state.by_key.insert(query.key, next);
                    state.pending.insert(next, replacement);
                }
                Err(failure) => {
                    state.by_key.remove(&query.key);
                    send_result(
                        results,
                        result_waker,
                        ResolveResult {
                            key: query.key,
                            result: Err(failure),
                        },
                    );
                }
            }
        }
        Ok(ParsedAnswer::Canonical(_)) => {
            state.by_key.remove(&query.key);
            send_result(
                results,
                result_waker,
                ResolveResult {
                    key: query.key,
                    result: Err(ResolveFailure::new(
                        "the DNS CNAME chain exceeds the private hop limit",
                    )),
                },
            );
        }
        Ok(ParsedAnswer::NoRecords) if query.record_type == RecordType::A => {
            match prepare_name_query(
                query.key,
                query.host,
                RecordType::AAAA,
                query.cname_hops,
                &state.pending,
                &mut state.next_id,
            ) {
                Ok((next, replacement)) => {
                    state.by_key.insert(query.key, next);
                    state.pending.insert(next, replacement);
                }
                Err(failure) => {
                    state.by_key.remove(&query.key);
                    send_result(
                        results,
                        result_waker,
                        ResolveResult {
                            key: query.key,
                            result: Err(failure),
                        },
                    );
                }
            }
        }
        Ok(ParsedAnswer::NoRecords) => {
            state.by_key.remove(&query.key);
            send_result(
                results,
                result_waker,
                ResolveResult {
                    key: query.key,
                    result: Err(ResolveFailure::new(
                        "the DNS response contained no usable A or AAAA records",
                    )),
                },
            );
        }
        Err(failure) => {
            state.by_key.remove(&query.key);
            send_result(
                results,
                result_waker,
                ResolveResult {
                    key: query.key,
                    result: Err(failure),
                },
            );
        }
        Ok(ParsedAnswer::Truncated) => {
            state.by_key.remove(&query.key);
            send_result(
                results,
                result_waker,
                ResolveResult {
                    key: query.key,
                    result: Err(ResolveFailure::new(
                        "the DNS-over-TCP response was unexpectedly truncated",
                    )),
                },
            );
        }
    }
}

fn begin_tcp_fallback(
    id: u16,
    query: &mut PendingQuery,
    poll: &Poll,
    nameserver: SocketAddr,
    timeout: Duration,
    tcp_by_token: &mut HashMap<Token, u16>,
    next_tcp_token: &mut usize,
) -> Result<(), ResolveFailure> {
    let length = u16::try_from(query.wire.len())
        .map_err(|_| ResolveFailure::new("the DNS query is too large for TCP framing"))?;
    let token = Token(*next_tcp_token);
    *next_tcp_token = next_tcp_token
        .checked_add(1)
        .ok_or_else(|| ResolveFailure::new("the native resolver TCP token space is exhausted"))?;
    let mut stream = TcpStream::connect(nameserver)
        .map_err(|error| ResolveFailure::new(format!("DNS-over-TCP connect failed: {error}")))?;
    poll.registry()
        .register(
            &mut stream,
            token,
            Interest::READABLE.add(Interest::WRITABLE),
        )
        .map_err(|error| {
            ResolveFailure::new(format!("DNS-over-TCP registration failed: {error}"))
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

fn remove_query(id: u16, state: &mut ResolverState, poll: &Poll) -> Option<PendingQuery> {
    let mut query = state.pending.remove(&id)?;
    if let QueryTransport::Tcp(tcp) = &mut query.transport {
        state.tcp_by_token.remove(&tcp.token);
        let _deregister_result = poll.registry().deregister(&mut tcp.stream);
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
    poll: &Poll,
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
                let _deregister_result = poll.registry().deregister(&mut tcp.stream);
            }
            let parsed =
                parse_answer(&message, id, &query.host, query.record_type).unwrap_or_else(|| {
                    Err(ResolveFailure::new(
                        "the DNS-over-TCP response did not match its query",
                    ))
                });
            finish_answer(query, parsed, state, results, result_waker);
        }
        Err(failure) => {
            if let QueryTransport::Tcp(tcp) = &mut query.transport {
                state.tcp_by_token.remove(&tcp.token);
                let _deregister_result = poll.registry().deregister(&mut tcp.stream);
            }
            state.by_key.remove(&query.key);
            send_result(
                results,
                result_waker,
                ResolveResult {
                    key: query.key,
                    result: Err(failure),
                },
            );
        }
    }
}

fn drive_tcp(
    tcp: &mut TcpFallback,
    readable: bool,
    writable: bool,
    poll: &Poll,
) -> Result<TcpDrive, ResolveFailure> {
    if writable && tcp.written < tcp.outbound.len() {
        if let Some(error) = tcp.stream.take_error().map_err(|error| {
            ResolveFailure::new(format!("DNS-over-TCP connect status failed: {error}"))
        })? {
            return Err(ResolveFailure::new(format!(
                "DNS-over-TCP connect failed: {error}"
            )));
        }
        while tcp.written < tcp.outbound.len() {
            match tcp.stream.write(&tcp.outbound[tcp.written..]) {
                Ok(0) => {
                    return Err(ResolveFailure::new(
                        "DNS-over-TCP closed while sending the query",
                    ));
                }
                Ok(written) => tcp.written += written,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    return Err(ResolveFailure::new(format!(
                        "DNS-over-TCP query send failed: {error}"
                    )));
                }
            }
        }
        if tcp.written == tcp.outbound.len() {
            poll.registry()
                .reregister(&mut tcp.stream, tcp.token, Interest::READABLE)
                .map_err(|error| {
                    ResolveFailure::new(format!("DNS-over-TCP re-registration failed: {error}"))
                })?;
        }
    }
    if readable {
        let mut buffer = [0_u8; 1024];
        loop {
            match tcp.stream.read(&mut buffer) {
                Ok(0) => {
                    return Err(ResolveFailure::new(
                        "DNS-over-TCP closed before a complete response",
                    ));
                }
                Ok(read) => {
                    if tcp.inbound.len().saturating_add(read) > DNS_PACKET_LIMIT + 2 {
                        return Err(ResolveFailure::new(
                            "DNS-over-TCP response exceeds the private packet limit",
                        ));
                    }
                    tcp.inbound.extend_from_slice(&buffer[..read]);
                    if tcp.expected.is_none() && tcp.inbound.len() >= 2 {
                        let expected =
                            usize::from(u16::from_be_bytes([tcp.inbound[0], tcp.inbound[1]]));
                        if expected == 0 || expected > DNS_PACKET_LIMIT {
                            return Err(ResolveFailure::new(
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
                    return Err(ResolveFailure::new(format!(
                        "DNS-over-TCP response read failed: {error}"
                    )));
                }
            }
        }
    }
    Ok(TcpDrive::Pending)
}

enum ParsedAnswer {
    Answer(ResolveAnswer),
    Canonical(Name),
    NoRecords,
    Truncated,
}

fn parse_answer(
    bytes: &[u8],
    expected_id: u16,
    expected_name: &Name,
    expected_type: RecordType,
) -> Option<Result<ParsedAnswer, ResolveFailure>> {
    let message = match Message::from_vec(bytes) {
        Ok(message) => message,
        Err(_) => {
            return Some(Err(ResolveFailure::new(
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
    if message.response_code() != ResponseCode::NoError {
        return Some(Err(ResolveFailure::new(format!(
            "the DNS server returned {:?}",
            message.response_code()
        ))));
    }
    let mut accepted_names = HashSet::from([expected_name.clone()]);
    let mut canonical_target = None;
    let mut ttl = u32::MAX;
    for _ in 0..MAX_CNAME_HOPS {
        let mut added = false;
        for answer in message.answers() {
            if accepted_names.contains(answer.name()) {
                if let RData::CNAME(canonical) = answer.data() {
                    if accepted_names.insert(canonical.0.clone()) {
                        ttl = ttl.min(answer.ttl());
                        canonical_target = Some(canonical.0.clone());
                        added = true;
                    }
                }
            }
        }
        if !added {
            break;
        }
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
            return Some(Ok(ParsedAnswer::Canonical(canonical)));
        }
        return Some(Ok(ParsedAnswer::NoRecords));
    }
    Some(Ok(ParsedAnswer::Answer(ResolveAnswer {
        addresses,
        ttl: Duration::from_secs(u64::from(ttl)),
    })))
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
        send_result(
            results,
            result_waker,
            ResolveResult {
                key: query.key,
                result: Err(ResolveFailure::new(message)),
            },
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

    use hickory_proto::rr::rdata::{A, AAAA, CNAME};
    use hickory_proto::rr::{RData, Record};
    use rcgen::{CertificateParams, KeyPair};
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::{ServerConfig, ServerConnection, StreamOwned};

    use super::*;
    use crate::backend::native::NativeReactor;
    use crate::{Completion, EngineConfig, ExecuteError, Request, TlsVerification, TransportStage};

    struct DnsFixture {
        address: SocketAddr,
        stop: Sender<()>,
        joined: Option<JoinHandle<()>>,
    }

    struct ScriptedDnsFixture {
        address: SocketAddr,
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
            parse_answer(&cname_wire, 41, &alias, RecordType::A)
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
            parse_answer(&aaaa_wire, 42, &ipv6_name, RecordType::AAAA)
        else {
            panic!("AAAA response must resolve");
        };
        assert_eq!(answer.addresses, vec![IpAddr::V6(ipv6)]);
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
            parse_answer(&wire, 51, &name, RecordType::A),
            Some(Ok(ParsedAnswer::Truncated))
        ));

        let mut wrong = Message::new();
        wrong
            .set_id(52)
            .set_message_type(MessageType::Response)
            .add_query(Query::query(fqdn("other.test"), RecordType::A));
        let wire = wrong.to_vec().expect("wrong response must encode");
        assert!(parse_answer(&wire, 52, &name, RecordType::A).is_none());
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

    #[test]
    fn truncated_udp_response_falls_back_to_fragmented_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("DNS TCP fixture must bind");
        listener
            .set_nonblocking(true)
            .expect("DNS TCP listener must be nonblocking");
        let address = listener.local_addr().expect("DNS TCP fixture address");
        let udp = StdUdpSocket::bind(address).expect("DNS UDP fixture must share the TCP port");
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
        let listener = TcpListener::bind("127.0.0.1:0").expect("DNS TCP barrier must bind");
        listener
            .set_nonblocking(true)
            .expect("DNS TCP barrier must be nonblocking");
        let address = listener.local_addr().expect("DNS TCP barrier address");
        let udp = StdUdpSocket::bind(address).expect("DNS UDP barrier must share the TCP port");
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
        let answer = wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("second DNS server must resolve");
        assert_eq!(
            answer.addresses,
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 31))]
        );
        resolver
            .shutdown()
            .expect("multi-server resolver must join");
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
                    Ok(0) => return started.elapsed(),
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                        panic!("TLS peer was not closed after cancellation")
                    }
                    Err(_) => return started.elapsed(),
                }
            }
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
        let peer_close = server.join().expect("TLS stall must join");
        assert!(peer_close < Duration::from_millis(500));
        let started = Instant::now();
        engine.shutdown().expect("TLS stall Engine must stop");
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}
