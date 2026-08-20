#![cfg_attr(not(test), allow(dead_code))]

//! Private Engine-owned nonblocking DNS service.
//!
//! Hickory is used only for DNS wire encoding and decoding. NBReq owns the socket, poll loop,
//! retry clock, command/result queues, cancellation, wakeup, and joined shutdown.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, MessageType, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};

use super::native::NativeWaker;
use crate::{Error, ErrorKind};

const WAKE_TOKEN: Token = Token(0);
const SOCKET_TOKEN: Token = Token(1);
const DNS_PACKET_LIMIT: usize = 4096;
const DEFAULT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct ResolveKey(pub(super) u64);

#[derive(Clone, Copy, Debug)]
pub(super) struct ResolverConfig {
    pub(super) nameserver: SocketAddr,
    pub(super) attempt_timeout: Duration,
    pub(super) attempts: u8,
}

impl ResolverConfig {
    pub(super) fn injected(nameserver: SocketAddr) -> Self {
        Self {
            nameserver,
            attempt_timeout: DEFAULT_ATTEMPT_TIMEOUT,
            attempts: DEFAULT_ATTEMPTS,
        }
    }

    #[cfg(test)]
    fn for_test(nameserver: SocketAddr) -> Self {
        let mut config = Self::injected(nameserver);
        config.attempt_timeout = Duration::from_millis(50);
        config.attempts = 2;
        config
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
    wire: Vec<u8>,
    attempts_sent: u8,
    next_attempt: Instant,
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
        let bind_address = match config.nameserver {
            SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        };
        let mut socket = UdpSocket::bind(bind_address)
            .map_err(|error| resolver_internal("socket bind", &error))?;
        socket
            .connect(config.nameserver)
            .map_err(|error| resolver_internal("nameserver connect", &error))?;
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
                    u16::from_ne_bytes(initial_id),
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
    mut next_id: u16,
) {
    let mut events = Events::with_capacity(16);
    let mut pending = HashMap::<u16, PendingQuery>::new();
    let mut by_key = HashMap::<ResolveKey, u16>::new();
    loop {
        let mut stop = false;
        loop {
            match commands.try_recv() {
                Ok(Command::Resolve { key, host }) => {
                    if let Some(previous) = by_key.remove(&key) {
                        pending.remove(&previous);
                    }
                    match prepare_query(&host, &pending, &mut next_id) {
                        Ok((id, query)) => {
                            by_key.insert(key, id);
                            pending.insert(
                                id,
                                PendingQuery {
                                    key,
                                    host: query.0,
                                    wire: query.1,
                                    attempts_sent: 0,
                                    next_attempt: Instant::now(),
                                },
                            );
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
                    if let Some(id) = by_key.remove(&key) {
                        pending.remove(&id);
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

        transmit_due(
            socket,
            &mut pending,
            &mut by_key,
            &results,
            &result_waker,
            config,
        );
        let timeout = pending
            .values()
            .map(|query| query.next_attempt.saturating_duration_since(Instant::now()))
            .min();
        if poll.poll(&mut events, timeout).is_err() {
            fail_all(
                &mut pending,
                &mut by_key,
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
            receive_packets(socket, &mut pending, &mut by_key, &results, &result_waker);
        }
    }
}

fn prepare_query(
    host: &str,
    pending: &HashMap<u16, PendingQuery>,
    next_id: &mut u16,
) -> Result<(u16, (Name, Vec<u8>)), ResolveFailure> {
    let mut name =
        Name::from_ascii(host).map_err(|_| ResolveFailure::new("the DNS hostname is invalid"))?;
    name.set_fqdn(true);
    let id = allocate_id(pending, next_id)
        .ok_or_else(|| ResolveFailure::new("the native resolver transaction space is exhausted"))?;
    let mut message = Message::new();
    message
        .set_id(id)
        .set_recursion_desired(true)
        .add_query(Query::query(name.clone(), RecordType::A));
    let wire = message
        .to_vec()
        .map_err(|_| ResolveFailure::new("the DNS query could not be encoded"))?;
    Ok((id, (name, wire)))
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
    pending: &mut HashMap<u16, PendingQuery>,
    by_key: &mut HashMap<ResolveKey, u16>,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
    config: ResolverConfig,
) {
    let now = Instant::now();
    let due = pending
        .iter()
        .filter_map(|(id, query)| (query.next_attempt <= now).then_some(*id))
        .collect::<Vec<_>>();
    for id in due {
        let exhausted = pending
            .get(&id)
            .is_some_and(|query| query.attempts_sent >= config.attempts);
        if exhausted {
            if let Some(query) = pending.remove(&id) {
                by_key.remove(&query.key);
                send_result(
                    results,
                    result_waker,
                    ResolveResult {
                        key: query.key,
                        result: Err(ResolveFailure::new(
                            "the DNS server did not answer within the retry budget",
                        )),
                    },
                );
            }
            continue;
        }
        let Some(query) = pending.get_mut(&id) else {
            continue;
        };
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

fn receive_packets(
    socket: &mut UdpSocket,
    pending: &mut HashMap<u16, PendingQuery>,
    by_key: &mut HashMap<ResolveKey, u16>,
    results: &Sender<ResolveResult>,
    result_waker: &NativeWaker,
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
        let Some(query) = pending.get(&id) else {
            continue;
        };
        let result = parse_answer(&buffer[..length], id, &query.host);
        let Some(result) = result else {
            continue;
        };
        let Some(query) = pending.remove(&id) else {
            continue;
        };
        by_key.remove(&query.key);
        send_result(
            results,
            result_waker,
            ResolveResult {
                key: query.key,
                result,
            },
        );
    }
}

fn parse_answer(
    bytes: &[u8],
    expected_id: u16,
    expected_name: &Name,
) -> Option<Result<ResolveAnswer, ResolveFailure>> {
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
        || message.queries()[0].query_type() != RecordType::A
    {
        return None;
    }
    if message.truncated() {
        return Some(Err(ResolveFailure::new(
            "the DNS response was truncated and TCP fallback is not implemented yet",
        )));
    }
    if message.response_code() != ResponseCode::NoError {
        return Some(Err(ResolveFailure::new(format!(
            "the DNS server returned {:?}",
            message.response_code()
        ))));
    }
    let mut addresses = Vec::new();
    let mut ttl = u32::MAX;
    for answer in message.answers() {
        if answer.name() == expected_name {
            if let RData::A(address) = answer.data() {
                addresses.push(IpAddr::V4(address.0));
                ttl = ttl.min(answer.ttl());
            }
        }
    }
    if addresses.is_empty() {
        return Some(Err(ResolveFailure::new(
            "the DNS response contained no usable A records",
        )));
    }
    Some(Ok(ResolveAnswer {
        addresses,
        ttl: Duration::from_secs(u64::from(ttl)),
    }))
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

    use hickory_proto::rr::rdata::A;
    use hickory_proto::rr::{RData, Record};

    use super::*;
    use crate::backend::native::NativeReactor;
    use crate::{Completion, EngineConfig, Request};

    struct DnsFixture {
        address: SocketAddr,
        stop: Sender<()>,
        joined: Option<JoinHandle<()>>,
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
}
