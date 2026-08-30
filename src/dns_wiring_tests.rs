#![cfg_attr(not(feature = "resolver"), allow(dead_code))]

use std::io::{self, Read, Write};
#[cfg(feature = "resolver")]
use std::net::IpAddr;
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
#[cfg(feature = "resolver")]
use std::num::NonZeroUsize;
#[cfg(feature = "resolver")]
use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::backend::native_dns_wire::test_support::{
    A, AAAA, CNAME, Message, MessageType, Name, RData, Record, RecordType, ResponseCode, SOA,
};

#[cfg(feature = "resolver")]
use crate::testing;
#[cfg(feature = "resolver")]
use crate::{
    AddressFamily, AddressOrder, CacheMode, DnsFailure, Engine, EngineConfig, ErrorKind,
    ExecuteError, Request, ResolveCompletion, ResolveRequest, ResolveStatus, ResolveWaitOutcome,
    TimeoutKind, TransportStage,
};

const DNS_PACKET_LIMIT: usize = 4096;

pub(crate) struct DualStackDns {
    address: std::net::SocketAddr,
    stop: mpsc::Sender<()>,
    queries: Arc<AtomicUsize>,
    qnames: Arc<Mutex<Vec<String>>>,
    joined: Option<JoinHandle<()>>,
}

impl DualStackDns {
    pub(crate) fn new() -> Self {
        Self::with_handler(move |request| Some(answer_dual_stack(&request)))
    }

    pub(crate) fn with_handler(
        mut handler: impl FnMut(Message) -> Option<Message> + Send + 'static,
    ) -> Self {
        Self::with_bytes(move |request| {
            handler(request).map(|response| {
                response
                    .to_vec()
                    .expect("public DNS fixture response must encode")
            })
        })
    }

    fn with_bytes(mut handler: impl FnMut(Message) -> Option<Vec<u8>> + Send + 'static) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("public DNS fixture must bind");
        socket
            .set_read_timeout(Some(Duration::from_millis(25)))
            .expect("public DNS fixture timeout must configure");
        let address = socket.local_addr().expect("public DNS fixture address");
        let (stop, stop_rx) = mpsc::channel();
        let queries = Arc::new(AtomicUsize::new(0));
        let qnames = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&queries);
        let seen_names = Arc::clone(&qnames);
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
                    // Winsock may report an ICMP port-unreachable from a resolver that
                    // has already closed as ConnectionReset on this shared UDP fixture.
                    // It says nothing about the fixture socket's ability to serve the next
                    // query, so keep listening just as we do after a read timeout.
                    #[cfg(windows)]
                    Err(error) if error.kind() == io::ErrorKind::ConnectionReset => continue,
                    Err(error) => panic!("public DNS fixture receive failed: {error}"),
                };
                seen.fetch_add(1, Ordering::SeqCst);
                let request = Message::from_vec(&buffer[..length])
                    .expect("public DNS fixture query must parse");
                if let Some(query) = request.query() {
                    seen_names.lock().expect("qname log").push(
                        query
                            .name()
                            .to_ascii()
                            .trim_end_matches('.')
                            .to_ascii_lowercase(),
                    );
                }
                if let Some(wire) = handler(request) {
                    socket
                        .send_to(&wire, peer)
                        .expect("public DNS fixture response must send");
                }
            }
        });
        Self {
            address,
            stop,
            queries,
            qnames,
            joined: Some(joined),
        }
    }

    fn qnames(&self) -> Vec<String> {
        self.qnames.lock().expect("qname log").clone()
    }

    pub(crate) fn address(&self) -> std::net::SocketAddr {
        self.address
    }

    pub(crate) fn queries(&self) -> usize {
        self.queries.load(Ordering::SeqCst)
    }
}

impl Drop for DualStackDns {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(joined) = self.joined.take() {
            joined.join().expect("public DNS fixture must join");
        }
    }
}

#[cfg(feature = "resolver")]
fn spawn_primed_http_listener(
    listener: TcpListener,
    priming_requests: usize,
) -> (mpsc::Sender<()>, Arc<AtomicUsize>, JoinHandle<()>) {
    let (release_tx, release_rx) = mpsc::channel();
    let stale_connections = Arc::new(AtomicUsize::new(0));
    let stale_seen = Arc::clone(&stale_connections);
    let joined = thread::spawn(move || {
        for _ in 0..priming_requests {
            let (mut stream, _) = listener.accept().expect("primed HTTP accept");
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nv1");
        }
        listener
            .set_nonblocking(true)
            .expect("primed HTTP listener must become nonblocking");
        loop {
            if release_rx.try_recv().is_ok() {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stale_seen.fetch_add(1, Ordering::SeqCst);
                    let mut buffer = [0_u8; 1024];
                    let _ = stream.read(&mut buffer);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nstale",
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("primed HTTP listener failed: {error}"),
            }
        }
    });
    (release_tx, stale_connections, joined)
}

enum TcpPayload {
    Message(Message),
    Truncated(Message),
    Raw(Vec<u8>),
    Hold,
}

struct TcpAwareDns {
    address: std::net::SocketAddr,
    stop: mpsc::Sender<()>,
    udp_queries: Arc<AtomicUsize>,
    tcp_queries: Arc<AtomicUsize>,
    tcp_query_ready: mpsc::Receiver<()>,
    joined: Option<JoinHandle<()>>,
}

impl TcpAwareDns {
    fn with_tcp(tcp: impl FnMut(Message) -> TcpPayload + Send + 'static) -> Self {
        let (listener, udp, address) = bind_dns_tcp_udp_pair();
        listener
            .set_nonblocking(true)
            .expect("public DNS TCP listener must be nonblocking");
        udp.set_read_timeout(Some(Duration::from_millis(25)))
            .expect("public DNS UDP timeout must configure");
        let (stop, stop_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(8);
        let udp_queries = Arc::new(AtomicUsize::new(0));
        let tcp_queries = Arc::new(AtomicUsize::new(0));
        let udp_seen = Arc::clone(&udp_queries);
        let tcp_seen = Arc::clone(&tcp_queries);
        let joined = thread::spawn(move || {
            run_tcp_aware_dns(listener, udp, stop_rx, ready_tx, udp_seen, tcp_seen, tcp);
        });
        Self {
            address,
            stop,
            udp_queries,
            tcp_queries,
            tcp_query_ready: ready_rx,
            joined: Some(joined),
        }
    }

    fn wait_until_tcp_query(&self) {
        self.tcp_query_ready
            .recv_timeout(Duration::from_secs(2))
            .expect("DNS-over-TCP query must be accepted and read");
    }
}

impl Drop for TcpAwareDns {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(joined) = self.joined.take() {
            joined.join().expect("public TCP DNS fixture must join");
        }
    }
}

fn bind_dns_tcp_udp_pair() -> (TcpListener, UdpSocket, std::net::SocketAddr) {
    for _ in 0..64 {
        let udp = UdpSocket::bind("127.0.0.1:0").expect("public DNS UDP must bind");
        let address = udp.local_addr().expect("public DNS UDP address");
        if let Ok(listener) = TcpListener::bind(address) {
            return (listener, udp, address);
        }
    }
    panic!("public DNS fixture could not reserve one port for UDP and TCP");
}

fn run_tcp_aware_dns(
    listener: TcpListener,
    udp: UdpSocket,
    stop_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::SyncSender<()>,
    udp_seen: Arc<AtomicUsize>,
    tcp_seen: Arc<AtomicUsize>,
    mut tcp: impl FnMut(Message) -> TcpPayload,
) {
    let mut buffer = [0_u8; DNS_PACKET_LIMIT];
    loop {
        if stop_rx.try_recv().is_ok() {
            return;
        }
        match udp.recv_from(&mut buffer) {
            Ok((length, peer)) => {
                udp_seen.fetch_add(1, Ordering::SeqCst);
                let request =
                    Message::from_vec(&buffer[..length]).expect("public DNS UDP query must parse");
                let query = request.query().expect("public DNS UDP query").clone();
                let mut truncated = Message::new();
                truncated
                    .set_id(request.id())
                    .set_message_type(MessageType::Response)
                    .set_truncated(true)
                    .add_query(query);
                let wire = truncated.to_vec().expect("truncated UDP reply must encode");
                udp.send_to(&wire, peer)
                    .expect("truncated UDP reply must send");
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("public DNS UDP receive failed: {error}"),
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .expect("public DNS TCP read timeout");
                stream
                    .set_write_timeout(Some(Duration::from_millis(250)))
                    .expect("public DNS TCP write timeout");
                let mut length = [0_u8; 2];
                if stream.read_exact(&mut length).is_err() {
                    continue;
                }
                let mut wire = vec![0_u8; usize::from(u16::from_be_bytes(length))];
                if stream.read_exact(&mut wire).is_err() {
                    continue;
                }
                let request = Message::from_vec(&wire).expect("public DNS TCP query must parse");
                tcp_seen.fetch_add(1, Ordering::SeqCst);
                let _ = ready_tx.try_send(());
                match tcp(request) {
                    TcpPayload::Hold => loop {
                        if stop_rx.try_recv().is_ok() {
                            return;
                        }
                        thread::sleep(Duration::from_millis(10));
                    },
                    TcpPayload::Raw(payload) => write_tcp_frame(&mut stream, &payload),
                    TcpPayload::Truncated(mut message) => {
                        message.set_truncated(true);
                        let payload = message.to_vec().expect("truncated TCP reply must encode");
                        write_tcp_frame(&mut stream, &payload);
                    }
                    TcpPayload::Message(message) => {
                        let payload = message.to_vec().expect("TCP DNS reply must encode");
                        write_tcp_frame(&mut stream, &payload);
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("public DNS TCP accept failed: {error}"),
        }
    }
}

fn write_tcp_frame(stream: &mut TcpStream, payload: &[u8]) {
    let length = u16::try_from(payload.len())
        .expect("TCP DNS payload must fit a length prefix")
        .to_be_bytes();
    let _ = stream.write_all(&length);
    let _ = stream.write_all(payload);
}

fn fqdn(name: &str) -> Name {
    let mut name = Name::from_ascii(name).expect("fixture DNS name must parse");
    name.set_fqdn(true);
    name
}

fn answer_dual_stack(request: &Message) -> Message {
    let query = request.query().expect("fixture query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .set_recursion_available(true)
        .add_query(query.clone());
    match query.query_type() {
        RecordType::A => {
            response.add_answer(Record::from_rdata(
                query.name().clone(),
                60,
                RData::A(A(Ipv4Addr::new(127, 0, 0, 10))),
            ));
        }
        RecordType::AAAA => {
            response.add_answer(Record::from_rdata(
                query.name().clone(),
                60,
                RData::AAAA(AAAA("2001:db8::10".parse().expect("fixture IPv6"))),
            ));
        }
        _ => {}
    }
    response
}

pub(crate) fn nxdomain(request: &Message) -> Message {
    let query = request.query().expect("NXDOMAIN query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .set_response_code(ResponseCode::NXDomain)
        .add_query(query)
        .add_name_server(Record::from_rdata(
            fqdn("test"),
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
}

pub(crate) fn nodata(request: &Message) -> Message {
    let query = request.query().expect("NoData query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .add_query(query)
        .add_name_server(Record::from_rdata(
            fqdn("test"),
            30,
            RData::SOA(SOA::new(
                fqdn("ns.test"),
                fqdn("hostmaster.test"),
                1,
                30,
                30,
                300,
                15,
            )),
        ));
    response
}

pub(crate) fn rcode(request: &Message, code: ResponseCode) -> Message {
    let query = request.query().expect("rcode query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .set_response_code(code)
        .add_query(query);
    response
}

fn malformed_wire(request: &Message) -> Vec<u8> {
    let mut wire = vec![0_u8; 12];
    wire[..2].copy_from_slice(&request.id().to_be_bytes());
    wire[2] = 0x80;
    wire[5] = 1;
    wire
}

pub(crate) fn a_record(request: &Message, ip: Ipv4Addr) -> Message {
    let query = request.query().expect("A query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .add_query(query.clone())
        .add_answer(Record::from_rdata(
            query.name().clone(),
            60,
            RData::A(A(ip)),
        ));
    response
}

pub(crate) fn aaaa_record(request: &Message, address: std::net::Ipv6Addr) -> Message {
    let query = request.query().expect("AAAA query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .add_query(query.clone())
        .add_answer(Record::from_rdata(
            query.name().clone(),
            60,
            RData::AAAA(AAAA(address)),
        ));
    response
}

fn cname_only(request: &Message, target: &str) -> Message {
    let query = request.query().expect("CNAME query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .add_query(query.clone())
        .add_answer(Record::from_rdata(
            query.name().clone(),
            30,
            RData::CNAME(CNAME(fqdn(target))),
        ));
    response
}

fn in_message_cname(request: &Message, canonical: &str, ip: Ipv4Addr) -> Message {
    let query = request.query().expect("in-message CNAME query").clone();
    let target = fqdn(canonical);
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .add_query(query.clone())
        .add_answer(Record::from_rdata(
            query.name().clone(),
            30,
            RData::CNAME(CNAME(target.clone())),
        ))
        .add_answer(Record::from_rdata(target, 20, RData::A(A(ip))));
    response
}

fn cname_chain(request: &Message, hop_names: &[&str], address: Option<Ipv4Addr>) -> Message {
    let query = request.query().expect("CNAME chain query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .add_query(query.clone());
    let mut current = query.name().clone();
    for name in hop_names {
        let next = fqdn(name);
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
    response
}

#[cfg(feature = "resolver")]
fn ipv4_request(name: &str) -> ResolveRequest {
    ResolveRequest::hostname(name)
        .address_family(AddressFamily::Ipv4)
        .build()
        .expect("IPv4 resolve request must build")
}

#[cfg(feature = "resolver")]
fn search_ipv4(name: &str) -> ResolveRequest {
    ResolveRequest::hostname(name)
        .address_family(AddressFamily::Ipv4)
        .use_search_suffixes(true)
        .build()
        .expect("search IPv4 resolve request must build")
}

#[cfg(feature = "resolver")]
fn public_dns_failure(engine: &Engine, name: &str) -> crate::Error {
    match engine.resolver().execute(ipv4_request(name)) {
        Err(ExecuteError::Failed(error)) => error,
        other => panic!("public lookup must fail: {other:?}"),
    }
}

#[cfg(feature = "resolver")]
fn http_dns_error(engine: &Engine, host: &str) -> crate::Error {
    match engine.client().execute(
        Request::get(format!("http://{host}/"))
            .connect_timeout(Duration::from_secs(1))
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP DNS request must build"),
    ) {
        Err(ExecuteError::Failed(error)) => error,
        other => panic!("HTTP DNS error must fail: {other:?}"),
    }
}

#[cfg(feature = "resolver")]
fn many_a(request: &Message, count: u16) -> Message {
    let query = request.query().expect("cap query").clone();
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .add_query(query.clone());
    for index in 1..=count {
        response.add_answer(Record::from_rdata(
            query.name().clone(),
            60,
            RData::A(A(Ipv4Addr::new(127, 0, 0, index as u8))),
        ));
    }
    response
}

#[cfg(feature = "resolver")]
fn spawned_engine(dns: &DualStackDns) -> Engine {
    testing::native_http_engine_with_nameserver(EngineConfig::spawned(), dns.address)
        .expect("native resolver Engine must construct")
}

#[cfg(feature = "resolver")]
fn spawned_engine_with_search(dns: &DualStackDns, suffixes: &[&str]) -> Engine {
    testing::native_http_engine_with_nameserver_and_search_suffixes(
        EngineConfig::spawned(),
        dns.address,
        suffixes.iter().copied(),
    )
    .expect("native resolver Engine with search suffixes must construct")
}

#[cfg(feature = "resolver")]
fn completed(engine: &Engine, request: ResolveRequest) -> crate::ResolveResponse {
    match engine.resolver().execute(request) {
        Ok(response) => response,
        Err(error) => panic!("resolution must complete: {error:?}"),
    }
}

#[test]
#[cfg(feature = "resolver")]
fn ipv4_ipv6_and_both_collect_from_local_fixture() {
    let dns = DualStackDns::new();
    let engine = spawned_engine(&dns);
    let resolver = engine.resolver();

    let ipv4 = completed(
        &engine,
        ResolveRequest::hostname("dual.test")
            .address_family(AddressFamily::Ipv4)
            .build()
            .expect("A request must build"),
    );
    assert_eq!(ipv4.name(), "dual.test");
    assert_eq!(ipv4.candidate_name(), Some("dual.test"));
    assert_eq!(ipv4.status(), ResolveStatus::Answer);
    assert_eq!(
        ipv4.addresses()
            .iter()
            .map(|address| address.address())
            .collect::<Vec<_>>(),
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 10))]
    );
    assert!(ipv4.valid_until().is_some());
    assert!(!ipv4.from_cache());

    let ipv6 = completed(
        &engine,
        ResolveRequest::hostname("dual.test")
            .address_family(AddressFamily::Ipv6)
            .cache_mode(CacheMode::Bypass)
            .build()
            .expect("AAAA request must build"),
    );
    assert_eq!(ipv6.status(), ResolveStatus::Answer);
    assert_eq!(
        ipv6.addresses()
            .iter()
            .map(|address| address.address())
            .collect::<Vec<_>>(),
        vec![IpAddr::V6("2001:db8::10".parse().expect("fixture IPv6"))]
    );

    let both = completed(
        &engine,
        ResolveRequest::hostname("dual.test")
            .cache_mode(CacheMode::Bypass)
            .build()
            .expect("Both request must build"),
    );
    assert_eq!(both.status(), ResolveStatus::Answer);
    assert_eq!(
        both.addresses()
            .iter()
            .map(|address| address.address())
            .collect::<Vec<_>>(),
        vec![
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 10)),
            IpAddr::V6("2001:db8::10".parse().expect("fixture IPv6")),
        ]
    );
    let _ = resolver;
    engine.shutdown().expect("Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn combined_family_order_is_applied_after_preserving_family_order() {
    let dns = DualStackDns::new();
    let engine = spawned_engine(&dns);

    let v4_first = completed(
        &engine,
        ResolveRequest::hostname("order.test")
            .address_order(AddressOrder::Ipv4ThenIpv6)
            .cache_mode(CacheMode::Bypass)
            .build()
            .expect("v4-first request must build"),
    );
    assert!(matches!(v4_first.addresses()[0].address(), IpAddr::V4(_)));
    assert!(matches!(v4_first.addresses()[1].address(), IpAddr::V6(_)));

    let v6_first = completed(
        &engine,
        ResolveRequest::hostname("order.test")
            .address_order(AddressOrder::Ipv6ThenIpv4)
            .cache_mode(CacheMode::Bypass)
            .build()
            .expect("v6-first request must build"),
    );
    assert!(matches!(v6_first.addresses()[0].address(), IpAddr::V6(_)));
    assert!(matches!(v6_first.addresses()[1].address(), IpAddr::V4(_)));
    engine.shutdown().expect("Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn nxdomain_and_nodata_are_completed_semantic_results() {
    let nx = DualStackDns::with_handler(|request| Some(nxdomain(&request)));
    let engine = spawned_engine(&nx);
    let missing = completed(
        &engine,
        ResolveRequest::hostname("missing.test")
            .build()
            .expect("NXDOMAIN request must build"),
    );
    assert_eq!(missing.status(), ResolveStatus::NameNotFound);
    assert_eq!(missing.candidate_name(), None);
    assert!(missing.addresses().is_empty());
    engine.shutdown().expect("NXDOMAIN Engine must stop");

    let empty = DualStackDns::with_handler(|request| Some(nodata(&request)));
    let engine = spawned_engine(&empty);
    let nodata = completed(
        &engine,
        ResolveRequest::hostname("empty.test")
            .address_family(AddressFamily::Ipv4)
            .build()
            .expect("NoData request must build"),
    );
    assert_eq!(nodata.status(), ResolveStatus::NoData);
    assert_eq!(nodata.candidate_name(), None);
    assert!(nodata.addresses().is_empty());
    engine.shutdown().expect("NoData Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn operational_failures_are_failed_with_payload_free_categories() {
    let servfail =
        DualStackDns::with_handler(|request| Some(rcode(&request, ResponseCode::ServFail)));
    let engine = spawned_engine(&servfail);
    match engine
        .resolver()
        .execute(
            ResolveRequest::hostname("fail.test")
                .address_family(AddressFamily::Ipv4)
                .build()
                .expect("SERVFAIL request must build"),
        )
        .expect_err("SERVFAIL must fail")
    {
        ExecuteError::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Transport);
            assert_eq!(error.transport_stage(), Some(TransportStage::Dns));
            assert_eq!(error.dns_failure(), Some(DnsFailure::ServerFailure));
        }
        other => panic!("expected Failed SERVFAIL, got {other:?}"),
    }
    engine.shutdown().expect("SERVFAIL Engine must stop");

    let refused =
        DualStackDns::with_handler(|request| Some(rcode(&request, ResponseCode::Refused)));
    let engine = spawned_engine(&refused);
    match engine
        .resolver()
        .execute(
            ResolveRequest::hostname("refused.test")
                .address_family(AddressFamily::Ipv4)
                .build()
                .expect("REFUSED request must build"),
        )
        .expect_err("REFUSED must fail")
    {
        ExecuteError::Failed(error) => {
            assert_eq!(error.dns_failure(), Some(DnsFailure::Refused));
        }
        other => panic!("expected Failed REFUSED, got {other:?}"),
    }
    engine.shutdown().expect("REFUSED Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn dns_over_tcp_connect_failure_is_classified_io() {
    let dns = DualStackDns::with_handler(|request| {
        let query = request.query().expect("truncated query").clone();
        let mut response = Message::new();
        response
            .set_id(request.id())
            .set_message_type(MessageType::Response)
            .set_truncated(true)
            .add_query(query);
        Some(response)
    });
    let engine = spawned_engine(&dns);
    match engine
        .resolver()
        .execute(
            ResolveRequest::hostname("tcp-io.test")
                .address_family(AddressFamily::Ipv4)
                .cache_mode(CacheMode::Bypass)
                .build()
                .expect("TCP I/O request must build"),
        )
        .expect_err("TCP connect failure must fail the public lookup")
    {
        ExecuteError::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Transport);
            assert_eq!(error.transport_stage(), Some(TransportStage::Dns));
            assert_eq!(error.dns_failure(), Some(DnsFailure::Io));
        }
        other => panic!("expected Failed Io, got {other:?}"),
    }
    engine.shutdown().expect("TCP I/O Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn formerr_and_malformed_are_failed_malformed_without_caching() {
    let formerr =
        DualStackDns::with_handler(|request| Some(rcode(&request, ResponseCode::FormErr)));
    let engine = spawned_engine(&formerr);
    let error = public_dns_failure(&engine, "formerr.test");
    assert_eq!(error.kind(), ErrorKind::Transport);
    assert_eq!(error.transport_stage(), Some(TransportStage::Dns));
    assert_eq!(error.dns_failure(), Some(DnsFailure::Malformed));
    let before = formerr.queries.load(Ordering::SeqCst);
    let again = public_dns_failure(&engine, "formerr.test");
    assert_eq!(again.dns_failure(), Some(DnsFailure::Malformed));
    assert!(formerr.queries.load(Ordering::SeqCst) > before);

    let http = http_dns_error(&engine, "formerr.test");
    assert_eq!(http.kind(), ErrorKind::Transport);
    assert_eq!(http.transport_stage(), Some(TransportStage::Dns));
    assert!(http.dns_failure().is_none());
    assert!(http.message().contains("FormErr") || http.message().contains("FORMERR"));
    engine.shutdown().expect("FORMERR Engine must stop");

    let malformed = DualStackDns::with_bytes(|request| Some(malformed_wire(&request)));
    let engine = spawned_engine(&malformed);
    let error = public_dns_failure(&engine, "malformed.test");
    assert_eq!(error.dns_failure(), Some(DnsFailure::Malformed));
    let before = malformed.queries.load(Ordering::SeqCst);
    let _ = public_dns_failure(&engine, "malformed.test");
    assert!(malformed.queries.load(Ordering::SeqCst) > before);
    let http = http_dns_error(&engine, "malformed.test");
    assert_eq!(http.transport_stage(), Some(TransportStage::Dns));
    assert!(http.dns_failure().is_none());
    engine.shutdown().expect("malformed Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn public_tcp_fallback_completes_and_classifies_bad_tcp_replies() {
    let success = TcpAwareDns::with_tcp(|request| {
        TcpPayload::Message(a_record(&request, Ipv4Addr::new(127, 0, 0, 21)))
    });
    let engine =
        testing::native_http_engine_with_nameserver(EngineConfig::spawned(), success.address)
            .expect("TCP success Engine must construct");
    let answered = completed(&engine, ipv4_request("tcp-ok.test"));
    assert_eq!(answered.status(), ResolveStatus::Answer);
    assert_eq!(
        answered.addresses()[0].address(),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 21))
    );
    assert!(!answered.from_cache());
    assert!(success.udp_queries.load(Ordering::SeqCst) >= 1);
    assert!(success.tcp_queries.load(Ordering::SeqCst) >= 1);
    let udp_before = success.udp_queries.load(Ordering::SeqCst);
    let tcp_before = success.tcp_queries.load(Ordering::SeqCst);
    let cached = completed(
        &engine,
        ResolveRequest::hostname("tcp-ok.test")
            .address_family(AddressFamily::Ipv4)
            .cache_mode(CacheMode::Use)
            .build()
            .expect("TCP cache request must build"),
    );
    assert!(cached.from_cache());
    assert_eq!(success.udp_queries.load(Ordering::SeqCst), udp_before);
    assert_eq!(success.tcp_queries.load(Ordering::SeqCst), tcp_before);
    engine.shutdown().expect("TCP success Engine must stop");

    let truncated = TcpAwareDns::with_tcp(|request| {
        TcpPayload::Truncated(a_record(&request, Ipv4Addr::new(127, 0, 0, 22)))
    });
    let engine =
        testing::native_http_engine_with_nameserver(EngineConfig::spawned(), truncated.address)
            .expect("TCP truncated Engine must construct");
    let error = public_dns_failure(&engine, "tcp-truncated.test");
    assert_eq!(error.dns_failure(), Some(DnsFailure::Truncated));
    let http = http_dns_error(&engine, "tcp-truncated.test");
    assert_eq!(http.transport_stage(), Some(TransportStage::Dns));
    assert!(http.dns_failure().is_none());
    engine.shutdown().expect("TCP truncated Engine must stop");

    let malformed = TcpAwareDns::with_tcp(|request| TcpPayload::Raw(malformed_wire(&request)));
    let engine =
        testing::native_http_engine_with_nameserver(EngineConfig::spawned(), malformed.address)
            .expect("TCP malformed Engine must construct");
    let error = public_dns_failure(&engine, "tcp-malformed.test");
    assert_eq!(error.dns_failure(), Some(DnsFailure::Malformed));
    let http = http_dns_error(&engine, "tcp-malformed.test");
    assert!(http.dns_failure().is_none());
    engine.shutdown().expect("TCP malformed Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn public_tcp_fallback_cancel_wins() {
    let held = TcpAwareDns::with_tcp(|_| TcpPayload::Hold);
    let engine = testing::native_http_engine_with_nameserver(EngineConfig::spawned(), held.address)
        .expect("TCP cancel Engine must construct");
    let pending = engine
        .resolver()
        .submit(ipv4_request("tcp-cancel.test"))
        .expect("TCP cancel resolution must submit");
    held.wait_until_tcp_query();
    assert!(held.tcp_queries.load(Ordering::SeqCst) >= 1);
    pending.handle().cancel().expect("TCP cancel must request");
    assert!(matches!(pending.wait(), ResolveCompletion::Cancelled));
    engine.shutdown().expect("TCP cancel Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn public_cname_chains_are_bounded_and_cache_hop_zero() {
    let in_message = DualStackDns::with_handler(|request| {
        Some(in_message_cname(
            &request,
            "canonical.test",
            Ipv4Addr::new(127, 0, 0, 9),
        ))
    });
    let engine = spawned_engine(&in_message);
    let first = completed(&engine, ipv4_request("alias.test"));
    assert_eq!(first.status(), ResolveStatus::Answer);
    assert_eq!(first.candidate_name(), Some("alias.test"));
    assert_eq!(
        first.addresses()[0].address(),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 9))
    );
    let before = in_message.queries.load(Ordering::SeqCst);
    let cached = completed(&engine, ipv4_request("alias.test"));
    assert!(cached.from_cache());
    assert_eq!(in_message.queries.load(Ordering::SeqCst), before);
    engine
        .shutdown()
        .expect("in-message CNAME Engine must stop");

    let follow = DualStackDns::with_handler(|request| {
        let query = request.query().expect("follow CNAME query").clone();
        if query.name() == &fqdn("follow.test") {
            Some(cname_only(&request, "target.test"))
        } else {
            assert_eq!(query.name(), &fqdn("target.test"));
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 12)))
        }
    });
    let engine = spawned_engine(&follow);
    let answered = completed(&engine, ipv4_request("follow.test"));
    assert_eq!(answered.candidate_name(), Some("follow.test"));
    assert_eq!(
        answered.addresses()[0].address(),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 12))
    );
    assert!(!answered.from_cache());
    let before = follow.queries.load(Ordering::SeqCst);
    let second = completed(&engine, ipv4_request("follow.test"));
    assert!(!second.from_cache());
    assert!(follow.queries.load(Ordering::SeqCst) > before);
    engine.shutdown().expect("follow CNAME Engine must stop");

    let root = DualStackDns::with_handler(|request| {
        let query = request.query().expect("root CNAME query").clone();
        let mut response = Message::new();
        response
            .set_id(request.id())
            .set_message_type(MessageType::Response)
            .add_query(query.clone())
            .add_answer(Record::from_rdata(
                query.name().clone(),
                30,
                RData::CNAME(CNAME(Name::root())),
            ));
        Some(response)
    });
    let engine = spawned_engine(&root);
    let error = public_dns_failure(&engine, "root-cname.test");
    assert_eq!(error.dns_failure(), Some(DnsFailure::Protocol));
    engine.shutdown().expect("root CNAME Engine must stop");

    let single = DualStackDns::with_handler(|request| {
        Some(cname_chain(
            &request,
            &[
                "n1.over.test",
                "n2.over.test",
                "n3.over.test",
                "n4.over.test",
                "n5.over.test",
                "n6.over.test",
                "n7.over.test",
                "n8.over.test",
                "n9.over.test",
            ],
            None,
        ))
    });
    let engine = spawned_engine(&single);
    let error = public_dns_failure(&engine, "over.test");
    assert_eq!(error.dns_failure(), Some(DnsFailure::Protocol));
    assert!(error.message().contains("CNAME chain exceeds"));
    let http = http_dns_error(&engine, "over.test");
    assert_eq!(http.transport_stage(), Some(TransportStage::Dns));
    assert!(http.dns_failure().is_none());
    assert!(http.message().contains("CNAME chain exceeds"));
    engine
        .shutdown()
        .expect("single-message CNAME overflow Engine must stop");

    let mixed = DualStackDns::with_handler(|request| {
        let query = request.query().expect("mixed CNAME query").clone();
        if query.name() == &fqdn("mixed.test") {
            Some(cname_chain(
                &request,
                &[
                    "m1.mixed.test",
                    "m2.mixed.test",
                    "m3.mixed.test",
                    "m4.mixed.test",
                    "m5.mixed.test",
                    "m6.mixed.test",
                ],
                None,
            ))
        } else {
            assert_eq!(query.name(), &fqdn("m6.mixed.test"));
            Some(cname_chain(
                &request,
                &["m7.mixed.test", "m8.mixed.test", "m9.mixed.test"],
                None,
            ))
        }
    });
    let engine = spawned_engine(&mixed);
    let error = public_dns_failure(&engine, "mixed.test");
    assert_eq!(error.dns_failure(), Some(DnsFailure::Protocol));
    engine
        .shutdown()
        .expect("mixed CNAME overflow Engine must stop");

    let mixed_ok = DualStackDns::with_handler(|request| {
        let query = request.query().expect("mixed-ok CNAME query").clone();
        if query.name() == &fqdn("okmix.test") {
            Some(cname_chain(
                &request,
                &[
                    "o1.okmix.test",
                    "o2.okmix.test",
                    "o3.okmix.test",
                    "o4.okmix.test",
                    "o5.okmix.test",
                    "o6.okmix.test",
                ],
                None,
            ))
        } else {
            assert_eq!(query.name(), &fqdn("o6.okmix.test"));
            Some(cname_chain(
                &request,
                &["o7.okmix.test", "o8.okmix.test"],
                Some(Ipv4Addr::new(127, 0, 0, 18)),
            ))
        }
    });
    let engine = spawned_engine(&mixed_ok);
    let answered = completed(&engine, ipv4_request("okmix.test"));
    assert_eq!(
        answered.addresses()[0].address(),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 18))
    );
    engine
        .shutdown()
        .expect("mixed CNAME success Engine must stop");

    let hops = DualStackDns::with_handler(|request| {
        let query = request.query().expect("hop CNAME query").clone();
        let ascii = query.name().to_ascii();
        let label = ascii.trim_end_matches('.');
        let index = label
            .strip_prefix("h")
            .and_then(|rest| rest.strip_suffix(".chain.test"))
            .and_then(|n| n.parse::<u8>().ok())
            .expect("hop CNAME name must parse");
        Some(cname_only(&request, &format!("h{}.chain.test", index + 1)))
    });
    let engine = spawned_engine(&hops);
    let error = public_dns_failure(&engine, "h0.chain.test");
    assert_eq!(error.dns_failure(), Some(DnsFailure::Protocol));
    assert!(error.message().contains("CNAME chain exceeds"));
    engine.shutdown().expect("CNAME hop Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn result_cap_is_enforced_before_unbounded_growth() {
    let dns = DualStackDns::with_handler(|request| Some(many_a(&request, 8)));
    let engine = spawned_engine(&dns);
    let capped = completed(
        &engine,
        ResolveRequest::hostname("cap.test")
            .address_family(AddressFamily::Ipv4)
            .max_results(3)
            .build()
            .expect("capped request must build"),
    );
    assert_eq!(capped.addresses().len(), 3);
    engine.shutdown().expect("capped Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn cache_use_refresh_and_bypass_are_honest() {
    let dns = DualStackDns::new();
    let engine = spawned_engine(&dns);
    let first = completed(
        &engine,
        ResolveRequest::hostname("cache.test")
            .address_family(AddressFamily::Ipv4)
            .cache_mode(CacheMode::Use)
            .build()
            .expect("cache Use request must build"),
    );
    assert!(!first.from_cache());
    let before = dns.queries.load(Ordering::SeqCst);

    let hit = completed(
        &engine,
        ResolveRequest::hostname("cache.test")
            .address_family(AddressFamily::Ipv4)
            .cache_mode(CacheMode::Use)
            .build()
            .expect("cache hit request must build"),
    );
    assert!(hit.from_cache());
    assert_eq!(hit.valid_until(), first.valid_until());
    assert_eq!(dns.queries.load(Ordering::SeqCst), before);

    let refreshed = completed(
        &engine,
        ResolveRequest::hostname("cache.test")
            .address_family(AddressFamily::Ipv4)
            .cache_mode(CacheMode::Refresh)
            .build()
            .expect("cache Refresh request must build"),
    );
    assert!(!refreshed.from_cache());
    assert!(dns.queries.load(Ordering::SeqCst) > before);

    let bypass_queries = dns.queries.load(Ordering::SeqCst);
    let bypassed = completed(
        &engine,
        ResolveRequest::hostname("cache.test")
            .address_family(AddressFamily::Ipv4)
            .cache_mode(CacheMode::Bypass)
            .build()
            .expect("cache Bypass request must build"),
    );
    assert!(!bypassed.from_cache());
    assert!(dns.queries.load(Ordering::SeqCst) > bypass_queries);
    engine.shutdown().expect("cache Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn ipv4_refresh_replaces_the_http_shared_cache_view() {
    let answer = Arc::new(Mutex::new(Ipv4Addr::LOCALHOST));
    let dns = DualStackDns::with_handler({
        let answer = Arc::clone(&answer);
        move |request| {
            let query = request.query().expect("HTTP-cache query").clone();
            let mut response = Message::new();
            response
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .add_query(query.clone());
            if query.query_type() == RecordType::A {
                let ip = *answer.lock().expect("HTTP-cache fixture address");
                response.add_answer(Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::A(A(ip)),
                ));
            }
            Some(response)
        }
    });
    let v1 = TcpListener::bind("127.0.0.1:0").expect("HTTP v1 listener");
    let port = v1.local_addr().expect("HTTP v1 port").port();
    let (release_server, stale_connections, server) = spawn_primed_http_listener(v1, 2);
    let engine = spawned_engine(&dns);
    let url = format!("http://shared-cache.test:{port}/");
    let first = engine
        .client()
        .execute(
            Request::get(url.clone())
                .connect_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("HTTP prime must build"),
        )
        .expect("HTTP prime must succeed");
    assert_eq!(first.body(), b"v1");

    *answer.lock().expect("HTTP-cache fixture address") = Ipv4Addr::new(127, 0, 0, 2);
    let stale = engine
        .client()
        .execute(
            Request::get(url.clone())
                .connect_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("HTTP stale cache must build"),
        )
        .expect("HTTP must keep the primed address before public refresh");
    assert_eq!(stale.body(), b"v1");

    let refreshed = completed(
        &engine,
        ResolveRequest::hostname("shared-cache.test")
            .address_family(AddressFamily::Ipv4)
            .cache_mode(CacheMode::Refresh)
            .build()
            .expect("public IPv4 refresh must build"),
    );
    assert!(!refreshed.from_cache());
    assert_eq!(
        refreshed.addresses()[0].address(),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))
    );

    let updated = engine.client().execute(
        Request::get(url)
            .connect_timeout(Duration::from_secs(1))
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP refreshed cache must build"),
    );
    release_server.send(()).expect("release primed HTTP server");
    server.join().expect("HTTP-cache server must join");
    assert!(
        updated.is_err(),
        "HTTP must stop using the reachable primed address after public IPv4 refresh"
    );
    assert_eq!(stale_connections.load(Ordering::SeqCst), 0);
    engine.shutdown().expect("HTTP-cache Engine must stop");
}

#[derive(Clone, Copy, Debug)]
#[cfg(feature = "resolver")]
enum Ipv6PublicReply {
    Answer,
    NoData,
    NameNotFound,
}

#[test]
#[cfg(feature = "resolver")]
fn ipv6_only_public_resolution_does_not_mutate_the_http_shared_cache_view() {
    for reply in [
        Ipv6PublicReply::Answer,
        Ipv6PublicReply::NoData,
        Ipv6PublicReply::NameNotFound,
    ] {
        ipv6_only_public_leaves_primed_http_a_view(reply);
    }
}

#[cfg(feature = "resolver")]
fn ipv6_only_public_leaves_primed_http_a_view(ipv6_reply: Ipv6PublicReply) {
    let a_answer = Arc::new(Mutex::new(Ipv4Addr::LOCALHOST));
    let a_queries = Arc::new(AtomicUsize::new(0));
    let dns = DualStackDns::with_handler({
        let a_answer = Arc::clone(&a_answer);
        let a_queries = Arc::clone(&a_queries);
        move |request| {
            let query = request.query().expect("HTTP-cache query").clone();
            match query.query_type() {
                RecordType::A => {
                    a_queries.fetch_add(1, Ordering::SeqCst);
                    let ip = *a_answer.lock().expect("HTTP-cache fixture address");
                    let mut response = Message::new();
                    response
                        .set_id(request.id())
                        .set_message_type(MessageType::Response)
                        .add_query(query.clone());
                    response.add_answer(Record::from_rdata(
                        query.name().clone(),
                        60,
                        RData::A(A(ip)),
                    ));
                    Some(response)
                }
                RecordType::AAAA => Some(match ipv6_reply {
                    Ipv6PublicReply::Answer => {
                        let mut response = Message::new();
                        response
                            .set_id(request.id())
                            .set_message_type(MessageType::Response)
                            .add_query(query.clone());
                        response.add_answer(Record::from_rdata(
                            query.name().clone(),
                            60,
                            RData::AAAA(AAAA("2001:db8::10".parse().expect("fixture IPv6"))),
                        ));
                        response
                    }
                    Ipv6PublicReply::NoData => nodata(&request),
                    Ipv6PublicReply::NameNotFound => nxdomain(&request),
                }),
                _ => None,
            }
        }
    });
    let v1 = TcpListener::bind("127.0.0.1:0").expect("HTTP v1 listener");
    let port = v1.local_addr().expect("HTTP v1 port").port();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = v1.accept().expect("HTTP v1 accept");
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nv1");
        }
    });
    let engine = spawned_engine(&dns);
    let host = format!("shared-cache-ipv6-{ipv6_reply:?}.test");
    let url = format!("http://{host}:{port}/");
    let first = engine
        .client()
        .execute(
            Request::get(url.clone())
                .connect_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("HTTP prime must build"),
        )
        .expect("HTTP prime must succeed");
    assert_eq!(first.body(), b"v1");
    let a_after_prime = a_queries.load(Ordering::SeqCst);
    assert_eq!(a_after_prime, 1, "HTTP prime must query A once");

    *a_answer.lock().expect("HTTP-cache fixture address") = Ipv4Addr::new(127, 0, 0, 2);
    let ipv6 = completed(
        &engine,
        ResolveRequest::hostname(host)
            .address_family(AddressFamily::Ipv6)
            .cache_mode(CacheMode::Refresh)
            .build()
            .expect("public IPv6 refresh must build"),
    );
    assert!(!ipv6.from_cache());
    match ipv6_reply {
        Ipv6PublicReply::Answer => {
            assert_eq!(ipv6.status(), ResolveStatus::Answer);
            assert_eq!(
                ipv6.addresses()[0].address(),
                IpAddr::V6("2001:db8::10".parse().expect("fixture IPv6"))
            );
        }
        Ipv6PublicReply::NoData => assert_eq!(ipv6.status(), ResolveStatus::NoData),
        Ipv6PublicReply::NameNotFound => assert_eq!(ipv6.status(), ResolveStatus::NameNotFound),
    }
    let cached = completed(
        &engine,
        ResolveRequest::hostname(ipv6.name())
            .address_family(AddressFamily::Ipv6)
            .cache_mode(CacheMode::Use)
            .build()
            .expect("public IPv6 cache-use must build"),
    );
    assert!(cached.from_cache());
    assert_eq!(cached.status(), ipv6.status());
    assert_eq!(
        a_queries.load(Ordering::SeqCst),
        a_after_prime,
        "IPv6-only public {ipv6_reply:?} must not query A"
    );

    let still_cached = engine
        .client()
        .execute(
            Request::get(url)
                .connect_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("HTTP after IPv6 must build"),
        )
        .unwrap_or_else(|error| {
            panic!("HTTP must keep primed A view after IPv6 {ipv6_reply:?}: {error:?}")
        });
    assert_eq!(still_cached.body(), b"v1");
    assert_eq!(
        a_queries.load(Ordering::SeqCst),
        a_after_prime,
        "HTTP after IPv6-only {ipv6_reply:?} must not issue another A query"
    );
    engine.shutdown().expect("IPv6 HTTP-cache Engine must stop");
    server.join().expect("IPv6 HTTP-cache server must join");
}

#[test]
#[cfg(feature = "resolver")]
fn both_refresh_replaces_the_http_shared_cache_view() {
    let answer = Arc::new(Mutex::new(Ipv4Addr::LOCALHOST));
    let dns = DualStackDns::with_handler({
        let answer = Arc::clone(&answer);
        move |request| {
            let query = request.query().expect("HTTP-cache query").clone();
            let mut response = Message::new();
            response
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .add_query(query.clone());
            match query.query_type() {
                RecordType::A => {
                    let ip = *answer.lock().expect("HTTP-cache fixture address");
                    response.add_answer(Record::from_rdata(
                        query.name().clone(),
                        60,
                        RData::A(A(ip)),
                    ));
                }
                RecordType::AAAA => {
                    response.add_answer(Record::from_rdata(
                        query.name().clone(),
                        60,
                        RData::AAAA(AAAA("2001:db8::10".parse().expect("fixture IPv6"))),
                    ));
                }
                _ => {}
            }
            Some(response)
        }
    });
    let v1 = TcpListener::bind("127.0.0.1:0").expect("HTTP v1 listener");
    let port = v1.local_addr().expect("HTTP v1 port").port();
    let (release_server, stale_connections, server) = spawn_primed_http_listener(v1, 2);
    let engine = spawned_engine(&dns);
    let url = format!("http://shared-cache-both.test:{port}/");
    let first = engine
        .client()
        .execute(
            Request::get(url.clone())
                .connect_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("HTTP prime must build"),
        )
        .expect("HTTP prime must succeed");
    assert_eq!(first.body(), b"v1");

    *answer.lock().expect("HTTP-cache fixture address") = Ipv4Addr::new(127, 0, 0, 2);
    let stale = engine
        .client()
        .execute(
            Request::get(url.clone())
                .connect_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("HTTP stale cache must build"),
        )
        .expect("HTTP must keep the primed address before public Both refresh");
    assert_eq!(stale.body(), b"v1");

    let refreshed = completed(
        &engine,
        ResolveRequest::hostname("shared-cache-both.test")
            .address_family(AddressFamily::Both)
            .cache_mode(CacheMode::Refresh)
            .build()
            .expect("public Both refresh must build"),
    );
    assert!(!refreshed.from_cache());
    assert_eq!(refreshed.status(), ResolveStatus::Answer);
    assert_eq!(
        refreshed.addresses()[0].address(),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))
    );

    let updated = engine.client().execute(
        Request::get(url)
            .connect_timeout(Duration::from_secs(1))
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP refreshed cache must build"),
    );
    release_server.send(()).expect("release primed HTTP server");
    server.join().expect("HTTP-cache server must join");
    assert!(
        updated.is_err(),
        "HTTP must stop using the reachable primed address after public Both refresh"
    );
    assert_eq!(stale_connections.load(Ordering::SeqCst), 0);
    engine.shutdown().expect("HTTP-cache Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn total_timeout_is_classified_from_acceptance() {
    let silent = UdpSocket::bind("127.0.0.1:0").expect("silent DNS must bind");
    let address = silent.local_addr().expect("silent DNS address");
    let engine = testing::native_http_engine_with_nameserver(EngineConfig::spawned(), address)
        .expect("timeout Engine must construct");
    match engine
        .resolver()
        .execute(
            ResolveRequest::hostname("slow.test")
                .address_family(AddressFamily::Ipv4)
                .total_timeout(Duration::from_millis(40))
                .build()
                .expect("timeout request must build"),
        )
        .expect_err("total timeout must fail")
    {
        ExecuteError::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Timeout);
            assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
            assert!(error.dns_failure().is_none());
        }
        other => panic!("expected timeout, got {other:?}"),
    }
    engine.shutdown().expect("timeout Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn expired_total_deadline_wins_over_cache_hit() {
    let dns = DualStackDns::new();
    let engine = spawned_engine(&dns);
    let primed = completed(
        &engine,
        ResolveRequest::hostname("deadline.test")
            .address_family(AddressFamily::Ipv4)
            .cache_mode(CacheMode::Use)
            .build()
            .expect("deadline prime must build"),
    );
    assert!(!primed.from_cache());
    match engine
        .resolver()
        .execute(
            ResolveRequest::hostname("deadline.test")
                .address_family(AddressFamily::Ipv4)
                .cache_mode(CacheMode::Use)
                .total_timeout(Duration::ZERO)
                .build()
                .expect("zero-timeout cache-hit request must build"),
        )
        .expect_err("expired deadline must beat a cache hit")
    {
        ExecuteError::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Timeout);
            assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
            assert!(error.dns_failure().is_none());
        }
        other => panic!("expected total timeout, got {other:?}"),
    }
    engine.shutdown().expect("deadline Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn expired_total_deadline_wins_over_queued_cache_hit() {
    let dns = DualStackDns::new();
    let mut engine =
        testing::native_http_engine_manual_with_nameserver(EngineConfig::manual(), dns.address)
            .expect("manual deadline Engine must construct");
    let primed = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("queued-deadline.test")
                .address_family(AddressFamily::Ipv4)
                .cache_mode(CacheMode::Use)
                .build()
                .expect("queued prime must build"),
        )
        .expect("queued prime must submit");
    match engine
        .drive_until(primed)
        .expect("queued prime must complete")
    {
        ResolveCompletion::Completed(_) => {}
        other => panic!("queued prime must be an answer: {other:?}"),
    }
    let pending = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("queued-deadline.test")
                .address_family(AddressFamily::Ipv4)
                .cache_mode(CacheMode::Use)
                .total_timeout(Duration::from_millis(1))
                .build()
                .expect("queued cache-hit request must build"),
        )
        .expect("queued cache-hit must submit");
    thread::sleep(Duration::from_millis(20));
    match engine
        .drive_until(pending)
        .expect("queued deadline drive must return")
    {
        ResolveCompletion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Timeout);
            assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
        }
        other => panic!("queued cache hit must not beat an expired deadline: {other:?}"),
    }
    engine.shutdown().expect("queued deadline Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn cancel_cancel_all_shutdown_and_exactly_one_terminal() {
    let silent = UdpSocket::bind("127.0.0.1:0").expect("cancel DNS must bind");
    silent
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("cancel DNS timeout");
    let address = silent.local_addr().expect("cancel DNS address");
    let engine = testing::native_http_engine_with_nameserver(EngineConfig::spawned(), address)
        .expect("cancel Engine must construct");
    let pending = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("cancel.test")
                .address_family(AddressFamily::Ipv4)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("cancel request must build"),
        )
        .expect("cancel resolution must submit");
    pending.handle().cancel().expect("individual cancel");
    assert!(matches!(pending.wait(), ResolveCompletion::Cancelled));

    let before = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("barrier.test")
                .address_family(AddressFamily::Ipv4)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("barrier request must build"),
        )
        .expect("barrier resolution must submit");
    engine.cancel_all();
    assert!(matches!(before.wait(), ResolveCompletion::Cancelled));
    let after = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("after-barrier.test")
                .address_family(AddressFamily::Ipv4)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("post-barrier request must build"),
        )
        .expect("post-barrier resolution must submit");
    match after.wait_for(Duration::ZERO) {
        ResolveWaitOutcome::TimedOut(live) => live.handle().cancel().expect("late cancel"),
        other => panic!("post-barrier resolution was terminal: {other:?}"),
    }

    for _ in 0..40 {
        let pending = engine
            .resolver()
            .submit(
                ResolveRequest::hostname("race.test")
                    .address_family(AddressFamily::Ipv4)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("race request must build"),
            )
            .expect("race resolution must submit");
        let handle = pending.handle();
        let barrier = Arc::new(Barrier::new(2));
        let cancel_barrier = Arc::clone(&barrier);
        let cancel_thread = thread::spawn(move || {
            cancel_barrier.wait();
            handle.cancel().expect("race cancel")
        });
        barrier.wait();
        let terminal = pending.wait();
        cancel_thread.join().expect("cancel thread");
        assert!(matches!(
            terminal,
            ResolveCompletion::Cancelled | ResolveCompletion::Failed(_)
        ));
    }

    let shutting = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("shutdown.test")
                .address_family(AddressFamily::Ipv4)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("shutdown request must build"),
        )
        .expect("shutdown resolution must submit");
    engine.shutdown().expect("Engine must stop");
    assert!(matches!(shutting.wait(), ResolveCompletion::Cancelled));
}

#[test]
#[cfg(feature = "resolver")]
fn waiter_local_timeout_leaves_the_operation_live() {
    let silent = UdpSocket::bind("127.0.0.1:0").expect("waiter DNS must bind");
    let address = silent.local_addr().expect("waiter DNS address");
    let engine = testing::native_http_engine_with_nameserver(EngineConfig::spawned(), address)
        .expect("waiter Engine must construct");
    let pending = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("wait.test")
                .address_family(AddressFamily::Ipv4)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("waiter request must build"),
        )
        .expect("waiter resolution must submit");
    match pending.wait_for(Duration::from_millis(15)) {
        ResolveWaitOutcome::TimedOut(live) => {
            assert!(!live.is_complete());
            live.handle()
                .cancel()
                .expect("live cancel after local wait");
            assert!(matches!(live.wait(), ResolveCompletion::Cancelled));
        }
        other => panic!("local wait must leave the resolution live: {other:?}"),
    }
    engine.shutdown().expect("waiter Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn callbacks_are_isolated_and_panic_is_contained() {
    let dns = DualStackDns::new();
    let engine = spawned_engine(&dns);
    let resolver = engine.resolver();
    let (tx, rx) = mpsc::channel();
    resolver
        .start(
            ResolveRequest::hostname("callback.test")
                .address_family(AddressFamily::Ipv4)
                .build()
                .expect("callback request must build"),
            move |completion| {
                tx.send(completion).expect("callback receiver");
            },
        )
        .expect("callback resolution must start");
    match rx.recv_timeout(Duration::from_secs(2)).expect("callback") {
        ResolveCompletion::Completed(response) => {
            assert_eq!(response.status(), ResolveStatus::Answer)
        }
        other => panic!("callback must complete: {other:?}"),
    }

    let panicking = resolver
        .start(
            ResolveRequest::hostname("panic.test")
                .address_family(AddressFamily::Ipv4)
                .cache_mode(CacheMode::Bypass)
                .build()
                .expect("panic request must build"),
            |_| panic!("deliberate public-resolver callback panic"),
        )
        .expect("panicking callback must start");
    thread::sleep(Duration::from_millis(50));
    let _ = panicking;

    let (survivor_tx, survivor_rx) = mpsc::channel();
    resolver
        .start(
            ResolveRequest::hostname("survivor.test")
                .address_family(AddressFamily::Ipv4)
                .cache_mode(CacheMode::Bypass)
                .build()
                .expect("survivor request must build"),
            move |completion| {
                survivor_tx.send(completion).expect("survivor receiver");
            },
        )
        .expect("survivor callback must start");
    survivor_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("dispatcher must survive callback panic");
    engine.shutdown().expect("callback Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn manual_drive_until_pending_resolve_and_wrong_engine_reject() {
    let dns = DualStackDns::new();
    let mut engine =
        testing::native_http_engine_manual_with_nameserver(EngineConfig::manual(), dns.address)
            .expect("manual resolver Engine must construct");
    let pending = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("manual.test")
                .address_family(AddressFamily::Ipv4)
                .build()
                .expect("manual request must build"),
        )
        .expect("manual resolution must submit");
    match engine
        .drive_until(pending)
        .expect("drive_until PendingResolve must complete")
    {
        ResolveCompletion::Completed(response) => {
            assert_eq!(response.status(), ResolveStatus::Answer)
        }
        other => panic!("manual drive must complete: {other:?}"),
    }

    let other = testing::engine(EngineConfig::manual()).expect("other Engine");
    let foreign = other
        .0
        .resolver()
        .submit(
            ResolveRequest::hostname("foreign.test")
                .build()
                .expect("foreign request must build"),
        )
        .expect_err("scaffold foreign resolver is unsupported");
    assert_eq!(foreign.kind(), ErrorKind::Unsupported);

    let mut second =
        testing::native_http_engine_manual_with_nameserver(EngineConfig::manual(), dns.address)
            .expect("second manual Engine");
    let pending = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("cross.test")
                .address_family(AddressFamily::Ipv4)
                .build()
                .expect("cross request must build"),
        )
        .expect("cross resolution must submit");
    let error = second
        .drive_until(pending)
        .expect_err("cross-Engine drive_until must fail");
    assert_eq!(error.kind(), ErrorKind::WrongEngine);
    engine.shutdown().expect("first manual Engine must stop");
    second.shutdown().expect("second manual Engine must stop");
    other.0.shutdown().expect("scaffold Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn wrong_engine_cancel_fails_closed() {
    let dns = DualStackDns::new();
    let first = spawned_engine(&dns);
    let pending = first
        .resolver()
        .submit(
            ResolveRequest::hostname("id.test")
                .address_family(AddressFamily::Ipv4)
                .build()
                .expect("id request must build"),
        )
        .expect("id resolution must submit");
    let id = pending.handle().id();
    let second = spawned_engine(&dns);
    let error = second
        .resolver()
        .cancel(id)
        .expect_err("cross-Engine cancel must fail");
    assert_eq!(error.kind(), ErrorKind::WrongEngine);
    first.shutdown().expect("first Engine must stop");
    second.shutdown().expect("second Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn public_saturation_does_not_starve_http_internal_dns() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fairness listener");
    let port = listener.local_addr().expect("HTTP fairness port").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP fairness accept");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    });
    let dns = DualStackDns::with_handler(move |request| {
        let query = request.query().expect("fairness query").clone();
        if query.name() == &fqdn("hold.test") {
            return None;
        }
        let mut response = Message::new();
        response
            .set_id(request.id())
            .set_message_type(MessageType::Response)
            .add_query(query.clone());
        if query.query_type() == RecordType::A {
            response.add_answer(Record::from_rdata(
                query.name().clone(),
                60,
                RData::A(A(Ipv4Addr::LOCALHOST)),
            ));
        }
        Some(response)
    });
    let config = EngineConfig::spawned()
        .with_max_inflight_resolutions(NonZeroUsize::new(1).expect("one public resolution"));
    let engine = testing::native_http_engine_with_nameserver(config, dns.address)
        .expect("fairness Engine must construct");
    let held = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("hold.test")
                .address_family(AddressFamily::Ipv4)
                .total_timeout(Duration::from_secs(3))
                .build()
                .expect("held request must build"),
        )
        .expect("held public resolution must submit");
    let saturated = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("second.test")
                .address_family(AddressFamily::Ipv4)
                .build()
                .expect("saturated request must build"),
        )
        .expect_err("second public resolution must saturate");
    assert_eq!(saturated.kind(), ErrorKind::QueueFull);

    let response = engine
        .client()
        .execute(
            Request::get(format!("http://http.test:{port}/"))
                .connect_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("HTTP DNS request must build"),
        )
        .expect("HTTP-internal DNS must complete while public resolution is saturated");
    assert_eq!(response.status(), 200);

    held.handle().cancel().expect("release held resolution");
    let _ = held.wait();
    engine.shutdown().expect("fairness Engine must stop");
    server.join().expect("HTTP fairness server must join");
}

#[test]
#[cfg(feature = "resolver")]
fn detached_resolver_returns_engine_stopped() {
    let dns = DualStackDns::new();
    let engine = spawned_engine(&dns);
    let resolver = engine.resolver();
    engine.shutdown().expect("Engine must stop");
    let error = resolver
        .submit(
            ResolveRequest::hostname("detached.test")
                .build()
                .expect("detached request must build"),
        )
        .expect_err("detached resolver must observe stop");
    assert_eq!(error.kind(), ErrorKind::EngineStopped);
}

#[test]
#[cfg(feature = "resolver")]
fn search_suffixes_follow_fq10_candidate_order_and_isolation() {
    let empty = DualStackDns::new();
    let engine = spawned_engine(&empty);
    let exact = completed(&engine, search_ipv4("www"));
    assert_eq!(exact.name(), "www");
    assert_eq!(exact.candidate_name(), Some("www"));
    assert_eq!(empty.qnames(), vec!["www".to_owned()]);
    engine
        .shutdown()
        .expect("empty-suffix search Engine must stop");

    let order = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.lab.test") || query.name() == &fqdn("www.svc") {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 31)))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&order, &["corp.test", "lab.test"]);
    let undotted = completed(&engine, search_ipv4("www"));
    assert_eq!(undotted.name(), "www");
    assert_eq!(undotted.candidate_name(), Some("www.lab.test"));
    assert_eq!(
        undotted.addresses()[0].address(),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 31))
    );
    assert_eq!(
        order.qnames(),
        vec!["www.corp.test".to_owned(), "www.lab.test".to_owned()]
    );
    assert!(!order.qnames().iter().any(|name| name == "www"));
    engine.shutdown().expect("search-order Engine must stop");

    let dotted = DualStackDns::with_handler(|request| {
        let query = request.query().expect("dotted search query").clone();
        if query.name() == &fqdn("www.svc") {
            Some(nxdomain(&request))
        } else if query.name() == &fqdn("www.svc.lab.test") {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 31)))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&dotted, &["corp.test", "lab.test"]);
    let answered = completed(&engine, search_ipv4("www.svc"));
    assert_eq!(answered.candidate_name(), Some("www.svc.lab.test"));
    assert_eq!(
        dotted.qnames(),
        vec![
            "www.svc".to_owned(),
            "www.svc.corp.test".to_owned(),
            "www.svc.lab.test".to_owned()
        ]
    );
    engine.shutdown().expect("dotted search Engine must stop");

    let trailing = DualStackDns::new();
    let engine = spawned_engine_with_search(&trailing, &["corp.test", "lab.test"]);
    let absolute = completed(
        &engine,
        ResolveRequest::hostname("www.")
            .use_search_suffixes(true)
            .address_family(AddressFamily::Ipv4)
            .build()
            .expect("absolute search request must build"),
    );
    assert_eq!(absolute.name(), "www");
    assert_eq!(absolute.candidate_name(), Some("www"));
    assert_eq!(trailing.qnames(), vec!["www".to_owned()]);
    engine
        .shutdown()
        .expect("trailing-dot search Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_continues_through_nodata_and_stops_on_operational_failure() {
    let mixed = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.corp.test") {
            Some(nodata(&request))
        } else if query.name() == &fqdn("www.lab.test") {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 32)))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&mixed, &["corp.test", "lab.test"]);
    let answered = completed(&engine, search_ipv4("www"));
    assert_eq!(answered.status(), ResolveStatus::Answer);
    assert_eq!(answered.candidate_name(), Some("www.lab.test"));
    assert_eq!(
        mixed.qnames(),
        vec!["www.corp.test".to_owned(), "www.lab.test".to_owned()]
    );
    engine.shutdown().expect("NoData-continue Engine must stop");

    let exhausted = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.lab.test") {
            Some(nodata(&request))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&exhausted, &["corp.test", "lab.test"]);
    let negative = completed(&engine, search_ipv4("www"));
    assert_eq!(negative.status(), ResolveStatus::NoData);
    assert_eq!(negative.candidate_name(), None);
    assert!(negative.valid_until().is_some());
    engine
        .shutdown()
        .expect("exhausted-NoData Engine must stop");

    let failed = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.corp.test") {
            Some(rcode(&request, ResponseCode::ServFail))
        } else {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 33)))
        }
    });
    let engine = spawned_engine_with_search(&failed, &["corp.test", "lab.test"]);
    let error = match engine.resolver().execute(search_ipv4("www")) {
        Err(ExecuteError::Failed(error)) => error,
        other => panic!("SERVFAIL must fail the search: {other:?}"),
    };
    assert_eq!(error.dns_failure(), Some(DnsFailure::ServerFailure));
    assert_eq!(failed.qnames(), vec!["www.corp.test".to_owned()]);
    engine.shutdown().expect("SERVFAIL search Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_cache_keys_follow_the_winning_candidate_and_from_cache_is_whole_search() {
    let dns = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.lab.test") {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 34)))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&dns, &["corp.test", "lab.test"]);
    let first = completed(&engine, search_ipv4("www"));
    assert!(!first.from_cache());
    assert_eq!(first.candidate_name(), Some("www.lab.test"));
    let network_queries = dns.queries.load(Ordering::SeqCst);

    let cached = completed(&engine, search_ipv4("www"));
    assert!(cached.from_cache());
    assert_eq!(cached.candidate_name(), Some("www.lab.test"));
    assert_eq!(dns.queries.load(Ordering::SeqCst), network_queries);

    let exact = completed(&engine, ipv4_request("www.lab.test"));
    assert!(exact.from_cache());
    assert_eq!(exact.candidate_name(), Some("www.lab.test"));
    assert_eq!(dns.queries.load(Ordering::SeqCst), network_queries);

    let relative = completed(&engine, ipv4_request("www"));
    assert!(!relative.from_cache());
    assert_eq!(relative.status(), ResolveStatus::NameNotFound);
    assert_eq!(relative.candidate_name(), None);
    assert!(dns.queries.load(Ordering::SeqCst) > network_queries);
    engine.shutdown().expect("search-cache Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_does_not_expand_http_lookups() {
    let dns = DualStackDns::with_handler(|request| {
        let query = request.query().expect("isolation query").clone();
        if query.name() == &fqdn("www.lab.test") {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 35)))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&dns, &["corp.test", "lab.test"]);
    let _public = completed(&engine, search_ipv4("www"));
    let http = match engine.client().execute(
        Request::get("http://www/")
            .connect_timeout(Duration::from_secs(1))
            .total_timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP isolation request must build"),
    ) {
        Err(ExecuteError::Failed(error)) => error,
        other => panic!("HTTP to www must stay an exact-name DNS failure: {other:?}"),
    };
    assert_eq!(http.transport_stage(), Some(TransportStage::Dns));
    assert!(dns.qnames().iter().any(|name| name == "www"));
    engine.shutdown().expect("HTTP isolation Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_cancel_during_a_later_candidate_does_not_promote_a_negative() {
    let hold = Arc::new(Barrier::new(2));
    let gate = Arc::clone(&hold);
    let dns = DualStackDns::with_handler(move |request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.corp.test") {
            Some(nxdomain(&request))
        } else {
            gate.wait();
            None
        }
    });
    let engine = spawned_engine_with_search(&dns, &["corp.test", "lab.test"]);
    let pending = engine
        .resolver()
        .submit(search_ipv4("www"))
        .expect("held search must submit");
    hold.wait();
    pending
        .handle()
        .cancel()
        .expect("search cancel must request");
    assert!(matches!(pending.wait(), ResolveCompletion::Cancelled));
    engine.shutdown().expect("search-cancel Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_exhausted_nxdomain_is_name_not_found() {
    let dns = DualStackDns::with_handler(|request| Some(nxdomain(&request)));
    let engine = spawned_engine_with_search(&dns, &["corp.test", "lab.test"]);
    let negative = completed(&engine, search_ipv4("www"));
    assert_eq!(negative.status(), ResolveStatus::NameNotFound);
    assert_eq!(negative.candidate_name(), None);
    assert_eq!(
        dns.qnames(),
        vec!["www.corp.test".to_owned(), "www.lab.test".to_owned()]
    );
    engine
        .shutdown()
        .expect("exhausted-NXDOMAIN Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_skips_oversized_suffixes_and_keeps_valid_candidates() {
    let long = "a".repeat(250);
    let dns = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.lab.test") {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 36)))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&dns, &[long.as_str(), "lab.test"]);
    let answered = completed(&engine, search_ipv4("www"));
    assert_eq!(answered.candidate_name(), Some("www.lab.test"));
    assert_eq!(dns.qnames(), vec!["www.lab.test".to_owned()]);
    engine
        .shutdown()
        .expect("oversized-suffix Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_refresh_and_bypass_requery_candidates() {
    let dns = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.lab.test") {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 37)))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&dns, &["corp.test", "lab.test"]);
    let first = completed(&engine, search_ipv4("www"));
    assert!(!first.from_cache());
    let after_first = dns.queries.load(Ordering::SeqCst);

    let hit = completed(&engine, search_ipv4("www"));
    assert!(hit.from_cache());
    assert_eq!(dns.queries.load(Ordering::SeqCst), after_first);

    let refreshed = completed(
        &engine,
        ResolveRequest::hostname("www")
            .address_family(AddressFamily::Ipv4)
            .use_search_suffixes(true)
            .cache_mode(CacheMode::Refresh)
            .build()
            .expect("search Refresh request must build"),
    );
    assert!(!refreshed.from_cache());
    assert_eq!(refreshed.candidate_name(), Some("www.lab.test"));
    assert!(dns.queries.load(Ordering::SeqCst) > after_first);
    let after_refresh = dns.queries.load(Ordering::SeqCst);

    let bypassed = completed(
        &engine,
        ResolveRequest::hostname("www")
            .address_family(AddressFamily::Ipv4)
            .use_search_suffixes(true)
            .cache_mode(CacheMode::Bypass)
            .build()
            .expect("search Bypass request must build"),
    );
    assert!(!bypassed.from_cache());
    assert_eq!(bypassed.candidate_name(), Some("www.lab.test"));
    assert!(dns.queries.load(Ordering::SeqCst) > after_refresh);
    engine
        .shutdown()
        .expect("search Refresh/Bypass Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_from_cache_is_false_when_any_candidate_uses_the_network() {
    let dns = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.lab.test") {
            Some(a_record(&request, Ipv4Addr::new(127, 0, 0, 38)))
        } else {
            Some(nxdomain(&request))
        }
    });
    let engine = spawned_engine_with_search(&dns, &["corp.test", "lab.test"]);
    let primed = completed(&engine, ipv4_request("www.corp.test"));
    assert_eq!(primed.status(), ResolveStatus::NameNotFound);
    let after_prime = dns.queries.load(Ordering::SeqCst);

    let mixed = completed(&engine, search_ipv4("www"));
    assert_eq!(mixed.status(), ResolveStatus::Answer);
    assert!(!mixed.from_cache());
    assert_eq!(mixed.candidate_name(), Some("www.lab.test"));
    assert_eq!(dns.qnames()[after_prime..], ["www.lab.test".to_owned()]);
    engine
        .shutdown()
        .expect("mixed-cache search Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn search_total_timeout_during_a_later_candidate_does_not_promote_a_negative() {
    let dns = DualStackDns::with_handler(|request| {
        let query = request.query().expect("search query").clone();
        if query.name() == &fqdn("www.corp.test") {
            Some(nxdomain(&request))
        } else {
            None
        }
    });
    let engine = spawned_engine_with_search(&dns, &["corp.test", "lab.test"]);
    match engine
        .resolver()
        .execute(
            ResolveRequest::hostname("www")
                .address_family(AddressFamily::Ipv4)
                .use_search_suffixes(true)
                .total_timeout(Duration::from_millis(80))
                .build()
                .expect("search timeout request must build"),
        )
        .expect_err("total timeout must fail the search")
    {
        ExecuteError::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Timeout);
            assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
            assert!(error.dns_failure().is_none());
        }
        other => panic!("expected total timeout, got {other:?}"),
    }
    assert_eq!(
        dns.qnames().first().map(String::as_str),
        Some("www.corp.test")
    );
    engine.shutdown().expect("search-timeout Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn http_unknown_host_remains_transport_dns_without_dns_failure() {
    let nx = DualStackDns::with_handler(|request| Some(nxdomain(&request)));
    let engine = spawned_engine(&nx);
    match engine
        .client()
        .execute(
            Request::get("http://missing.test/")
                .connect_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("HTTP unknown-host request must build"),
        )
        .expect_err("HTTP unknown host must fail")
    {
        ExecuteError::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Transport);
            assert_eq!(error.transport_stage(), Some(TransportStage::Dns));
            assert!(error.dns_failure().is_none());
            assert!(error.message().contains("NXDomain"));
        }
        other => panic!("HTTP unknown host must stay a transport DNS error: {other:?}"),
    }
    engine
        .shutdown()
        .expect("HTTP unknown-host Engine must stop");
}

#[test]
#[cfg(feature = "resolver")]
fn curl_less_scaffold_and_native_without_dns_stay_unsupported() {
    let engine = Engine::with_backend(EngineConfig::spawned(), crate::backend::scaffold())
        .expect("scaffold Engine");
    let error = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("example.com")
                .build()
                .expect("scaffold request must build"),
        )
        .expect_err("scaffold has no public resolver");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    engine.shutdown().expect("scaffold Engine must stop");

    let proving = testing::native_http_engine(EngineConfig::spawned())
        .expect("native proving Engine without DNS");
    let error = proving
        .resolver()
        .submit(
            ResolveRequest::hostname("example.com")
                .build()
                .expect("proving request must build"),
        )
        .expect_err("proving Engine without DNS owner has no public resolver");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    proving.shutdown().expect("proving Engine must stop");
}
