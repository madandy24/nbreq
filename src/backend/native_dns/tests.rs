use std::io::{Read, Write};
use std::net::TcpListener;
use std::net::UdpSocket as StdUdpSocket;
use std::sync::mpsc as test_channel;

use super::dns_wire::test_support::{A, AAAA, CNAME, Record, SOA};
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
        include_bytes!("../../../fuzz/corpus/native_dns_response/a.seed").as_slice(),
        include_bytes!("../../../fuzz/corpus/native_dns_response/aaaa.seed").as_slice(),
        include_bytes!("../../../fuzz/corpus/native_dns_response/cname.seed").as_slice(),
        include_bytes!("../../../fuzz/corpus/native_dns_response/nxdomain.seed").as_slice(),
        include_bytes!("../../../fuzz/corpus/native_dns_response/root-cname.seed").as_slice(),
        include_bytes!("../../../fuzz/corpus/native_dns_response/truncated.seed").as_slice(),
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
    observed: Arc<Mutex<Vec<Name>>>,
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
                let request =
                    Message::from_vec(&buffer[..length]).expect("scripted DNS request must parse");
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
        let observed = Arc::new(Mutex::new(Vec::new()));
        let thread_observed = Arc::clone(&observed);
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
                thread_observed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(query.name().clone());
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
            observed,
            joined: Some(joined),
        }
    }

    fn observations(&self) -> Vec<Name> {
        self.observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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

fn dns_name(name: &str) -> DnsName {
    DnsName::from_ascii(name).expect("fixture NBReq DNS name must parse")
}

fn parse_fixture_wire(
    bytes: &[u8],
    id: u16,
    name: &Name,
    record_type: RecordType,
    remaining_cname_hops: u8,
) -> Option<Result<ParsedAnswer, ResolveFailure>> {
    let name = dns_name(&name.to_utf8());
    let record_type = match record_type {
        RecordType::A => DnsRecordType::A,
        RecordType::AAAA => DnsRecordType::AAAA,
        other => panic!("unsupported fixture record type {other:?}"),
    };
    parse_answer(bytes, id, &name, record_type, remaining_cname_hops)
}

#[test]
fn production_query_encoder_has_one_question_and_no_records() {
    let name = dns_name("bounded-query.test");
    let mut next_id = 41;
    let (id, query) = prepare_name_query(
        ResolveKey(9),
        name.clone(),
        DnsRecordType::A,
        0,
        &HashMap::new(),
        &mut next_id,
        QueryPolicy::Http(RetryPolicy {
            attempt_limit: DEFAULT_ATTEMPTS,
            attempt_timeout: DEFAULT_ATTEMPT_TIMEOUT,
        }),
    )
    .expect("ordinary DNS query must encode");
    let message = Message::from_vec(&query.wire).expect("ordinary DNS query must decode");

    assert_eq!(message.id(), id);
    assert_eq!(message.queries().len(), 1);
    assert_eq!(message.queries()[0].name().to_utf8(), name.to_ascii());
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
        .add_query(Query::new(alias.clone(), RecordType::A))
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
        parse_fixture_wire(&cname_wire, 41, &alias, RecordType::A, MAX_CNAME_HOPS)
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
        .add_query(Query::new(ipv6_name.clone(), RecordType::AAAA))
        .add_answer(Record::from_rdata(
            ipv6_name.clone(),
            60,
            RData::AAAA(AAAA(ipv6)),
        ));
    let aaaa_wire = aaaa_response.to_vec().expect("AAAA response must encode");
    let Some(Ok(ParsedAnswer::Answer(answer))) =
        parse_fixture_wire(&aaaa_wire, 42, &ipv6_name, RecordType::AAAA, MAX_CNAME_HOPS)
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
        .add_query(Query::new(alias.clone(), RecordType::A))
        .add_answer(Record::from_rdata(
            alias.clone(),
            30,
            RData::CNAME(CNAME(Name::root())),
        ));
    let wire = response.to_vec().expect("root-CNAME response must encode");

    let Some(Err(failure)) = parse_fixture_wire(&wire, 43, &alias, RecordType::A, MAX_CNAME_HOPS)
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
        .add_query(Query::new(start.clone(), RecordType::A));
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
    let Some(Err(failure)) = parse_fixture_wire(&over, 61, &start, RecordType::A, MAX_CNAME_HOPS)
    else {
        panic!("nine in-message CNAME links must exceed the hop budget");
    };
    assert_eq!(
        failure.message,
        "the DNS CNAME chain exceeds the private hop limit"
    );

    let exact = cname_chain_wire(62, &start, MAX_CNAME_HOPS, None);
    let Some(Ok(ParsedAnswer::Canonical { hops, .. })) =
        parse_fixture_wire(&exact, 62, &start, RecordType::A, MAX_CNAME_HOPS)
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
        parse_fixture_wire(&answered, 63, &start, RecordType::A, MAX_CNAME_HOPS)
    else {
        panic!("eight in-message CNAME links plus an address must complete");
    };
    assert_eq!(
        answer.addresses,
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 63))]
    );

    let leftover = cname_chain_wire(64, &start, 3, None);
    let Some(Err(failure)) = parse_fixture_wire(&leftover, 64, &start, RecordType::A, 2) else {
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
        .add_query(Query::new(name.clone(), RecordType::A));
    let wire = truncated.to_vec().expect("truncated response must encode");
    assert!(matches!(
        parse_fixture_wire(&wire, 51, &name, RecordType::A, MAX_CNAME_HOPS),
        Some(Ok(ParsedAnswer::Truncated))
    ));

    let mut wrong = Message::new();
    wrong
        .set_id(52)
        .set_message_type(MessageType::Response)
        .add_query(Query::new(fqdn("other.test"), RecordType::A));
    let wire = wrong.to_vec().expect("wrong response must encode");
    assert!(parse_fixture_wire(&wire, 52, &name, RecordType::A, MAX_CNAME_HOPS).is_none());
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
    let mut config = ResolverConfig::for_test(address);
    // The fixture deliberately fragments a valid TCP reply across three writes. Keep the
    // resolver deadline bounded without making loaded-runner scheduling part of that proof.
    config.attempt_timeout = Duration::from_secs(1);
    let mut resolver =
        NativeResolver::new(config, owner.waker()).expect("TCP-fallback resolver must construct");
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
    let answering = StdUdpSocket::bind("127.0.0.1:0").expect("answering DNS server must bind");
    answering
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("answering DNS server timeout must configure");
    let answering_address = answering
        .local_addr()
        .expect("answering DNS server address");
    let config = ResolverConfig::multiple_for_test(vec![silent_address, answering_address]);
    let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
    let mut resolver =
        NativeResolver::new(config, owner.waker()).expect("multi-server resolver must construct");
    resolver
        .resolve(ResolveKey(80), "rotate.test".to_owned())
        .expect("multi-server resolution must submit");

    // Serve the second socket on this test thread. A spawned fixture can be starved by the rest of
    // the parallel suite after rotation, turning scheduler delay into a false retry-budget failure.
    let mut buffer = [0_u8; DNS_PACKET_LIMIT];
    let (length, peer) = answering
        .recv_from(&mut buffer)
        .expect("query must rotate to the second DNS server");
    let request = Message::from_vec(&buffer[..length]).expect("rotated DNS query must parse");
    let query = request
        .query()
        .expect("rotated DNS query must exist")
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
            RData::A(A(Ipv4Addr::new(127, 0, 0, 31))),
        ));
    answering
        .send_to(
            &response.to_vec().expect("rotated DNS response must encode"),
            peer,
        )
        .expect("rotated DNS response must send");

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
        unavailable_is_unsupported: true,
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
fn live_configuration_refresh_moves_the_original_resolver_from_a_to_b() {
    let first = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 51));
    let second = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 52));
    let (config, updates) = ResolverConfig::for_test(first.address).with_test_refresh_source();
    let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
    let mut resolver =
        NativeResolver::new(config, owner.waker()).expect("refreshable resolver must construct");

    resolver
        .resolve(ResolveKey(210), "configuration-a.test".to_owned())
        .expect("first-generation lookup must submit");
    assert_eq!(
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("first generation must answer")
            .addresses,
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 51))]
    );

    updates
        .send(Ok(ResolverSnapshot {
            nameservers: vec![second.address],
            search_suffixes: Vec::new(),
            attempt_timeout: Duration::from_millis(50),
            attempts: 2,
        }))
        .expect("test configuration B must publish");
    resolver
        .refresh_config_for_test()
        .expect("test refresh must wake the owner");
    assert!(
        resolver
            .take_idle_http_eviction()
            .expect("configuration controls must remain live")
            .is_some(),
        "route replacement must request idle HTTP eviction"
    );
    resolver
        .resolve(ResolveKey(211), "configuration-a.test".to_owned())
        .expect("second-generation lookup must submit");
    assert_eq!(
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("refreshed generation must answer")
            .addresses,
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 52))]
    );
    assert!(
        second
            .observations()
            .contains(&fqdn("configuration-a.test")),
        "a route-generation change must clear the old positive cache entry"
    );

    resolver.shutdown().expect("refreshable resolver must join");
}

#[test]
fn equal_live_configuration_snapshot_is_a_complete_no_op() {
    let fixture = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 53));
    let snapshot = ResolverSnapshot {
        nameservers: vec![fixture.address],
        search_suffixes: Vec::new(),
        attempt_timeout: Duration::from_millis(50),
        attempts: 2,
    };
    let (config, updates) = ResolverConfig::for_test(fixture.address)
        .with_test_refresh_source_and_interval(Some(Duration::from_millis(5)));
    let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
    let mut resolver =
        NativeResolver::new(config, owner.waker()).expect("refreshable resolver must construct");

    resolver
        .resolve(ResolveKey(212), "configuration-equal.test".to_owned())
        .expect("prime lookup must submit");
    wait_for_resolution(&mut owner, &resolver)
        .result
        .expect("prime lookup must answer");
    updates
        .send(Ok(snapshot.clone()))
        .expect("equal test configuration must publish");
    resolver
        .refresh_config_for_test()
        .expect("equal refresh must complete");
    assert!(
        resolver
            .take_idle_http_eviction()
            .expect("configuration controls must remain live")
            .is_none(),
        "an equal snapshot must not evict idle HTTP"
    );
    updates
        .send(Ok(snapshot))
        .expect("duplicate equal configuration must publish");
    resolver
        .refresh_config_for_test()
        .expect("duplicate equal refresh must complete");
    thread::sleep(Duration::from_millis(20));
    assert!(
        resolver
            .take_idle_http_eviction()
            .expect("configuration controls must remain live")
            .is_none(),
        "duplicate notifications and periodic equal refreshes must be no-ops"
    );

    resolver
        .resolve(ResolveKey(213), "configuration-equal.test".to_owned())
        .expect("cached lookup must submit");
    wait_for_resolution(&mut owner, &resolver)
        .result
        .expect("equal refresh must preserve the cache");
    assert_eq!(
        fixture
            .observations()
            .iter()
            .filter(|name| **name == fqdn("configuration-equal.test"))
            .count(),
        1,
        "equal refresh must not clear a valid cache entry"
    );

    resolver.shutdown().expect("refreshable resolver must join");
}

#[test]
fn route_change_fails_pending_once_without_replay_then_uses_the_new_server() {
    let silent = StdUdpSocket::bind("127.0.0.1:0").expect("silent DNS server must bind");
    silent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("silent DNS timeout must configure");
    let first_address = silent.local_addr().expect("silent DNS address");
    let second = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 54));
    let (config, updates) = ResolverConfig::for_test(first_address).with_test_refresh_source();
    let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
    let mut resolver =
        NativeResolver::new(config, owner.waker()).expect("refreshable resolver must construct");

    resolver
        .resolve(ResolveKey(214), "configuration-pending.test".to_owned())
        .expect("pending lookup must submit");
    let mut wire = [0_u8; DNS_PACKET_LIMIT];
    silent
        .recv_from(&mut wire)
        .expect("old server must observe the pending query");
    updates
        .send(Ok(ResolverSnapshot {
            nameservers: vec![second.address],
            search_suffixes: Vec::new(),
            attempt_timeout: Duration::from_millis(50),
            attempts: 2,
        }))
        .expect("configuration B must publish");
    resolver
        .refresh_config_for_test()
        .expect("route refresh must complete");
    let terminal = wait_for_resolution(&mut owner, &resolver);
    assert_eq!(terminal.key, ResolveKey(214));
    assert_eq!(
        terminal
            .result
            .expect_err("old-generation lookup must fail")
            .class,
        DnsFailure::NoNameserver
    );
    assert!(
        resolver
            .drain()
            .expect("result drain must stay live")
            .is_empty()
    );
    assert!(
        !second
            .observations()
            .contains(&fqdn("configuration-pending.test")),
        "the old-generation lookup must not replay on configuration B"
    );

    resolver
        .resolve(ResolveKey(215), "configuration-later.test".to_owned())
        .expect("new-generation lookup must submit");
    assert_eq!(
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("new generation must answer")
            .addresses,
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 54))]
    );
    resolver.shutdown().expect("refreshable resolver must join");
}

#[test]
fn unsupported_live_snapshot_fails_closed_and_later_recovers() {
    let first = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 55));
    let second = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 56));
    let (config, updates) = ResolverConfig::for_test(first.address).with_test_refresh_source();
    let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
    let mut resolver =
        NativeResolver::new(config, owner.waker()).expect("refreshable resolver must construct");

    updates
        .send(Err(Error::new(
            ErrorKind::Unsupported,
            "injected split DNS topology",
        )))
        .expect("unsupported topology must publish");
    resolver
        .refresh_config_for_test()
        .expect("unsupported refresh must complete");
    resolver
        .resolve(ResolveKey(216), "configuration-unavailable.test".to_owned())
        .expect("unavailable lookup must submit to the live Engine");
    assert_eq!(
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect_err("unsupported live topology must fail closed")
            .class,
        DnsFailure::NoNameserver
    );

    resolver
        .public_resolve(
            ResolveKey(218),
            PublicResolveSpec {
                host: "configuration-unavailable-public.test".to_owned(),
                family: AddressFamily::Ipv4,
                order: AddressOrder::Ipv4ThenIpv6,
                cache_mode: CacheMode::Bypass,
                max_results: 8,
                expand_search: false,
                unavailable_is_unsupported: true,
            },
        )
        .expect("unavailable public lookup must submit to the live Engine");
    let Some(PublicLookupOutcome::Failed(error)) =
        wait_for_resolution(&mut owner, &resolver).public
    else {
        panic!("unavailable public lookup must fail");
    };
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert_eq!(error.dns_failure(), None);

    resolver
        .public_resolve(
            ResolveKey(219),
            PublicResolveSpec {
                host: "configuration-unavailable-tcp.test".to_owned(),
                family: AddressFamily::Both,
                order: AddressOrder::Ipv4ThenIpv6,
                cache_mode: CacheMode::Use,
                max_results: 8,
                expand_search: false,
                unavailable_is_unsupported: false,
            },
        )
        .expect("unavailable hostname-TCP lookup must submit to the live Engine");
    let Some(PublicLookupOutcome::Failed(error)) =
        wait_for_resolution(&mut owner, &resolver).public
    else {
        panic!("unavailable hostname-TCP lookup must fail");
    };
    assert_eq!(error.kind(), ErrorKind::Transport);
    assert_eq!(error.dns_failure(), Some(DnsFailure::NoNameserver));

    updates
        .send(Ok(ResolverSnapshot {
            nameservers: vec![second.address],
            search_suffixes: Vec::new(),
            attempt_timeout: Duration::from_millis(50),
            attempts: 2,
        }))
        .expect("represented topology must publish");
    resolver
        .refresh_config_for_test()
        .expect("recovery refresh must complete");
    resolver
        .resolve(ResolveKey(217), "configuration-recovered.test".to_owned())
        .expect("recovered lookup must submit");
    assert_eq!(
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("live Engine must recover")
            .addresses,
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 56))]
    );

    resolver.shutdown().expect("refreshable resolver must join");
}

#[test]
fn injected_static_configuration_never_rediscovers() {
    let fixture = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 57));
    let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
    let mut resolver =
        NativeResolver::new(ResolverConfig::for_test(fixture.address), owner.waker())
            .expect("static resolver must construct");
    resolver
        .refresh_config_for_test()
        .expect("static refresh request must be harmless");
    assert!(
        resolver
            .take_idle_http_eviction()
            .expect("configuration controls must remain live")
            .is_none()
    );
    resolver
        .resolve(ResolveKey(218), "configuration-static.test".to_owned())
        .expect("static lookup must submit");
    assert_eq!(
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("static resolver must remain usable")
            .addresses,
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 57))]
    );
    resolver.shutdown().expect("static resolver must join");
}

#[test]
fn suffix_only_refresh_changes_future_public_candidates_without_route_churn() {
    let fixture = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 58));
    let (config, updates) = ResolverConfig::for_test(fixture.address)
        .with_search_suffixes(["corp.test"])
        .with_test_refresh_source();
    let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
    let mut resolver =
        NativeResolver::new(config, owner.waker()).expect("refreshable resolver must construct");
    let public = |host: &str| PublicResolveSpec {
        host: host.to_owned(),
        family: AddressFamily::Ipv4,
        order: AddressOrder::Ipv4ThenIpv6,
        cache_mode: CacheMode::Bypass,
        max_results: 8,
        expand_search: true,
        unavailable_is_unsupported: true,
    };

    resolver
        .public_resolve(ResolveKey(219), public("before"))
        .expect("first suffix lookup must submit");
    assert!(matches!(
        wait_for_resolution(&mut owner, &resolver).public,
        Some(PublicLookupOutcome::Completed {
            status: ResolveStatus::Answer,
            ..
        })
    ));
    updates
        .send(Ok(ResolverSnapshot {
            nameservers: vec![fixture.address],
            search_suffixes: vec!["lab.test".to_owned()],
            attempt_timeout: Duration::from_millis(50),
            attempts: 2,
        }))
        .expect("suffix-only configuration must publish");
    resolver
        .refresh_config_for_test()
        .expect("suffix-only refresh must complete");
    assert!(
        resolver
            .take_idle_http_eviction()
            .expect("configuration controls must remain live")
            .is_none(),
        "suffix-only refresh must not evict HTTP"
    );
    resolver
        .public_resolve(ResolveKey(220), public("after"))
        .expect("second suffix lookup must submit");
    assert!(matches!(
        wait_for_resolution(&mut owner, &resolver).public,
        Some(PublicLookupOutcome::Completed {
            status: ResolveStatus::Answer,
            ..
        })
    ));
    let observed = fixture.observations();
    assert!(observed.contains(&fqdn("before.corp.test")));
    assert!(observed.contains(&fqdn("after.lab.test")));
    assert!(!observed.contains(&fqdn("after.corp.test")));
    resolver.shutdown().expect("refreshable resolver must join");
}

#[test]
fn exhausted_transport_requests_the_same_configuration_transition() {
    let silent = StdUdpSocket::bind("127.0.0.1:0").expect("silent DNS server must bind");
    let first_address = silent.local_addr().expect("silent DNS address");
    let second = DnsFixture::answering(Ipv4Addr::new(127, 0, 0, 59));
    let (config, updates) = ResolverConfig::for_test(first_address).with_test_refresh_source();
    updates
        .send(Ok(ResolverSnapshot {
            nameservers: vec![second.address],
            search_suffixes: Vec::new(),
            attempt_timeout: Duration::from_millis(50),
            attempts: 2,
        }))
        .expect("configuration B must await exhaustion");
    let mut owner = NativeReactor::new(4).expect("owner reactor must construct");
    let mut resolver =
        NativeResolver::new(config, owner.waker()).expect("refreshable resolver must construct");

    resolver
        .resolve(ResolveKey(221), "configuration-exhausted.test".to_owned())
        .expect("exhausting lookup must submit");
    assert_eq!(
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect_err("the operation exposing exhaustion must still fail")
            .class,
        DnsFailure::NoNameserver
    );
    let transition_deadline = Instant::now() + Duration::from_secs(2);
    while resolver
        .take_idle_http_eviction()
        .expect("configuration controls must remain live")
        .is_none()
    {
        assert!(
            Instant::now() < transition_deadline,
            "exhaustion did not request rediscovery"
        );
        owner
            .poll(transition_deadline)
            .expect("transition wake poll must succeed");
    }
    resolver
        .resolve(
            ResolveKey(222),
            "configuration-after-exhaustion.test".to_owned(),
        )
        .expect("post-exhaustion lookup must submit");
    assert_eq!(
        wait_for_resolution(&mut owner, &resolver)
            .result
            .expect("later lookup must use configuration B")
            .addresses,
        vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 59))]
    );
    resolver.shutdown().expect("refreshable resolver must join");
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
        dns_name("zero.test"),
        Ok(answer.clone()),
        Duration::ZERO,
        MAX_POSITIVE_CACHE_TTL,
        now,
    );
    assert!(cache.entries.is_empty());
    let expiring = dns_name("expiring.test");
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
            dns_name(&format!("entry-{index}.test")),
            Ok(answer.clone()),
            MAX_POSITIVE_CACHE_TTL + Duration::from_secs(1),
            MAX_POSITIVE_CACHE_TTL,
            now,
        );
    }
    assert_eq!(cache.entries.len(), DNS_CACHE_CAPACITY);
    assert!(!cache.entries.contains_key(&dns_name("entry-0.test")));
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
    let engine =
        crate::testing::native_http_engine_with_nameserver(EngineConfig::spawned(), dns.address)
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
fn route_generation_change_evicts_idle_http_before_the_next_lease() {
    let dns_a = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let dns_b = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
    let address = listener.local_addr().expect("HTTP fixture address");
    let server = thread::spawn(move || {
        let read_request = |stream: &mut std::net::TcpStream| {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("HTTP fixture read timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("HTTP request must read");
                assert_ne!(read, 0, "client closed before HTTP request head");
                request.extend_from_slice(&buffer[..read]);
            }
        };
        let (mut first, _) = listener.accept().expect("first HTTP socket must accept");
        read_request(&mut first);
        first
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n1")
            .expect("first HTTP response must write");
        let mut closed = [0_u8; 1];
        assert_eq!(
            first
                .read(&mut closed)
                .expect("idle socket close must be observable"),
            0,
            "route generation change must close the idle socket"
        );
        let (mut second, _) = listener.accept().expect("second HTTP socket must accept");
        read_request(&mut second);
        second
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n2")
            .expect("second HTTP response must write");
    });

    let config = EngineConfig::spawned();
    let (factory, updates) =
        crate::backend::native_http::NativeHttpFactory::new_with_refreshable_nameserver(
            &config,
            dns_a.address,
            Duration::from_millis(5),
        );
    let engine = crate::Engine::with_spawned_factory(config, Box::new(factory))
        .expect("refreshable HTTP Engine must construct");
    let first = engine
        .client()
        .execute(
            Request::get(format!("http://refresh.test:{}/first", address.port()))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("first refresh request must build"),
        )
        .expect("first refresh request must complete");
    assert_eq!(first.body(), b"1");
    assert_eq!(engine.metrics().current().idle_connections(), 1);

    updates
        .send(Ok(ResolverConfig::snapshot_for_test(dns_b.address)))
        .expect("configuration B must publish");
    let refresh_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let metrics = engine.metrics();
        if metrics.current().idle_connections() == 0 && metrics.idle_connections_evicted() >= 1 {
            break;
        }
        assert!(
            Instant::now() < refresh_deadline,
            "route refresh did not evict the idle HTTP socket"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let second = engine
        .client()
        .execute(
            Request::get(format!("http://refresh.test:{}/second", address.port()))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("second refresh request must build"),
        )
        .expect("second refresh request must complete");
    assert_eq!(second.body(), b"2");
    let metrics = engine.metrics();
    assert_eq!(metrics.connections_opened(), 2);
    assert_eq!(metrics.connections_reused(), 0);
    assert!(
        dns_b.observations().contains(&fqdn("refresh.test")),
        "the second request must resolve through configuration B"
    );
    engine
        .shutdown()
        .expect("refreshable HTTP Engine must stop");
    server.join().expect("refreshable HTTP fixture must join");
}

#[cfg(feature = "resolver")]
#[test]
fn route_generation_change_leaves_an_active_http_response_alive() {
    let dns_a = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let dns_b = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let listener = TcpListener::bind("127.0.0.1:0").expect("active HTTP fixture must bind");
    let address = listener.local_addr().expect("active HTTP fixture address");
    let (body_started, body_started_rx) = test_channel::channel();
    let (release_body, release_body_rx) = test_channel::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("active HTTP socket must accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("active HTTP read timeout");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 512];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("active HTTP request read");
            assert_ne!(read, 0, "client closed before active HTTP request head");
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na")
            .expect("active HTTP response prefix must write");
        body_started
            .send(())
            .expect("active HTTP body-start signal must send");
        release_body_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("active HTTP body release must arrive");
        stream
            .write_all(b"b")
            .expect("active HTTP response suffix must write");
        let mut closed = [0_u8; 1];
        assert_eq!(
            stream
                .read(&mut closed)
                .expect("stale active HTTP socket close must be observable"),
            0,
            "an old-generation active HTTP socket must not enter the idle pool"
        );
    });

    let config = EngineConfig::spawned();
    let (factory, updates) =
        crate::backend::native_http::NativeHttpFactory::new_with_refreshable_nameserver(
            &config,
            dns_a.address,
            Duration::from_millis(5),
        );
    let engine = crate::Engine::with_spawned_factory(config, Box::new(factory))
        .expect("active HTTP refresh Engine must construct");
    let client = engine.client();
    let request_thread = thread::spawn(move || {
        client.execute(
            Request::get(format!("http://active-refresh.test:{}/", address.port()))
                .total_timeout(Duration::from_secs(3))
                .build()
                .expect("active HTTP refresh request must build"),
        )
    });
    body_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("active HTTP response must reach its body");

    updates
        .send(Ok(ResolverConfig::snapshot_for_test(dns_b.address)))
        .expect("active HTTP configuration B must publish");
    let refresh_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let response = engine.resolver().execute(
            crate::ResolveRequest::hostname(format!(
                "active-route-probe-{}.test",
                dns_b.observations().len()
            ))
            .address_family(crate::AddressFamily::Ipv4)
            .cache_mode(crate::CacheMode::Bypass)
            .total_timeout(Duration::from_millis(250))
            .build()
            .expect("active route probe must build"),
        );
        let response = match response {
            Ok(response) => response,
            Err(crate::ExecuteError::Failed(error))
                if error.dns_failure() == Some(DnsFailure::NoNameserver)
                    && Instant::now() < refresh_deadline =>
            {
                continue;
            }
            Err(error) => panic!("active route probe failed unexpectedly: {error:?}"),
        };
        if response
            .addresses()
            .iter()
            .any(|address| address.address() == IpAddr::V4(Ipv4Addr::LOCALHOST))
            && !dns_b.observations().is_empty()
        {
            break;
        }
        assert!(
            Instant::now() < refresh_deadline,
            "active HTTP route refresh did not reach configuration B"
        );
    }

    release_body
        .send(())
        .expect("active HTTP body must be released");
    let response = request_thread
        .join()
        .expect("active HTTP request thread must join")
        .expect("active HTTP request must survive the route change");
    assert_eq!(response.body(), b"ab");
    assert_eq!(
        engine.metrics().current().idle_connections(),
        0,
        "the old-generation active HTTP connection must close after completing"
    );
    engine
        .shutdown()
        .expect("active HTTP refresh Engine must stop");
    server.join().expect("active HTTP fixture must join");
}

#[cfg(feature = "resolver")]
#[test]
fn route_generation_change_leaves_live_tcp_alive_and_future_hostname_connect_uses_b() {
    use crate::TcpConnectRequest;

    let dns_a = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let dns_b = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let listener = TcpListener::bind("127.0.0.1:0").expect("active TCP fixture must bind");
    let port = listener
        .local_addr()
        .expect("active TCP fixture address")
        .port();
    let server = thread::spawn(move || {
        for expected in *b"ab" {
            let (mut stream, _) = listener.accept().expect("active TCP socket must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("active TCP read timeout");
            let mut byte = [0_u8; 1];
            stream
                .read_exact(&mut byte)
                .expect("active TCP byte must arrive");
            assert_eq!(byte[0], expected);
            stream.write_all(&byte).expect("active TCP echo must write");
        }
    });

    let config = EngineConfig::spawned();
    let (factory, updates) =
        crate::backend::native_http::NativeHttpFactory::new_with_refreshable_nameserver(
            &config,
            dns_a.address,
            Duration::from_millis(5),
        );
    let engine = crate::Engine::with_spawned_factory(config, Box::new(factory))
        .expect("active TCP refresh Engine must construct");
    let mut first = engine
        .tcp_connector()
        .execute(
            TcpConnectRequest::hostname("active-tcp-a.test", port)
                .connect_timeout(Duration::from_secs(2))
                .build()
                .expect("first hostname TCP request must build"),
        )
        .expect("first hostname TCP connect must complete");

    updates
        .send(Ok(ResolverConfig::snapshot_for_test(dns_b.address)))
        .expect("active TCP configuration B must publish");
    let refresh_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let probe_name = format!("tcp-route-probe-{}.test", dns_b.observations().len());
        let probe = engine.resolver().execute(
            crate::ResolveRequest::hostname(probe_name)
                .address_family(crate::AddressFamily::Ipv4)
                .cache_mode(crate::CacheMode::Bypass)
                .total_timeout(Duration::from_millis(250))
                .build()
                .expect("TCP route probe must build"),
        );
        match probe {
            Ok(_) if !dns_b.observations().is_empty() => break,
            Ok(_) => {}
            Err(crate::ExecuteError::Failed(error))
                if error.dns_failure() == Some(DnsFailure::NoNameserver)
                    && Instant::now() < refresh_deadline =>
            {
                continue;
            }
            Err(error) => panic!("TCP route probe failed unexpectedly: {error:?}"),
        }
        assert!(
            Instant::now() < refresh_deadline,
            "active TCP route refresh did not reach configuration B"
        );
    }

    first
        .send(vec![b'a'])
        .expect("first live TCP send must survive");
    let mut first_echo = [0_u8; 1];
    assert_eq!(
        first.read(&mut first_echo).expect("first live TCP read"),
        Some(1)
    );
    assert_eq!(first_echo, [b'a']);

    let mut second = engine
        .tcp_connector()
        .execute(
            TcpConnectRequest::hostname("active-tcp-b.test", port)
                .connect_timeout(Duration::from_secs(2))
                .build()
                .expect("second hostname TCP request must build"),
        )
        .expect("second hostname TCP connect must complete");
    second
        .send(vec![b'b'])
        .expect("second hostname TCP send must succeed");
    let mut second_echo = [0_u8; 1];
    assert_eq!(
        second
            .read(&mut second_echo)
            .expect("second hostname TCP read"),
        Some(1)
    );
    assert_eq!(second_echo, [b'b']);
    assert!(
        dns_b.observations().contains(&fqdn("active-tcp-b.test")),
        "a future hostname connect must resolve through configuration B"
    );
    drop(first);
    drop(second);
    engine.shutdown().expect("active TCP Engine must stop");
    server.join().expect("active TCP fixture must join");
}

#[test]
fn route_generation_change_fails_dns_pending_hostname_tcp_once_and_releases_borrows() {
    use crate::{DnsFailure, TcpConnectCompletion, TcpConnectRequest};

    let silent = StdUdpSocket::bind("127.0.0.1:0").expect("pending TCP DNS fixture must bind");
    silent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("pending TCP DNS read timeout");
    let first_address = silent.local_addr().expect("pending TCP DNS address");
    let second = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let config = EngineConfig::spawned();
    let (factory, updates) =
        crate::backend::native_http::NativeHttpFactory::new_with_refreshable_nameserver(
            &config,
            first_address,
            Duration::from_millis(5),
        );
    let engine = crate::Engine::with_spawned_factory(config, Box::new(factory))
        .expect("pending hostname TCP Engine must construct");
    let pending = engine
        .tcp_connector()
        .submit(
            TcpConnectRequest::hostname("pending-route-change.test", 9)
                .connect_timeout(Duration::from_secs(2))
                .build()
                .expect("pending hostname TCP request must build"),
        )
        .expect("pending hostname TCP request must submit");
    let mut wire = [0_u8; DNS_PACKET_LIMIT];
    silent
        .recv_from(&mut wire)
        .expect("configuration A must observe pending hostname DNS");

    updates
        .send(Ok(ResolverConfig::snapshot_for_test(second.address)))
        .expect("pending hostname TCP configuration B must publish");
    match pending.wait() {
        TcpConnectCompletion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Transport);
            assert_eq!(error.dns_failure(), Some(DnsFailure::NoNameserver));
        }
        other => panic!("pending hostname TCP must fail once at DNS transition: {other:?}"),
    }
    let metrics = engine.metrics();
    assert_eq!(metrics.tcp_connects_failed(), 1);
    assert_eq!(metrics.current().inflight_resolutions(), 0);
    assert_eq!(metrics.current().standalone_tcp_connections(), 0);
    assert_eq!(metrics.current().reserved_tcp_queue_bytes(), 0);
    engine
        .shutdown()
        .expect("pending hostname TCP Engine must stop");
}

#[cfg(feature = "resolver")]
#[test]
fn route_generation_change_and_public_cancel_commit_one_terminal_and_release_the_borrow() {
    use crate::{ResolveCompletion, ResolveRequest};

    let silent = StdUdpSocket::bind("127.0.0.1:0").expect("cancel-race DNS fixture must bind");
    silent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("cancel-race DNS read timeout");
    let first_address = silent.local_addr().expect("cancel-race DNS address");
    let second = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let config = EngineConfig::spawned();
    let (factory, updates) =
        crate::backend::native_http::NativeHttpFactory::new_with_refreshable_nameserver(
            &config,
            first_address,
            Duration::from_millis(5),
        );
    let engine = crate::Engine::with_spawned_factory(config, Box::new(factory))
        .expect("cancel-race Engine must construct");
    let pending = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("configuration-cancel-race.test")
                .address_family(crate::AddressFamily::Ipv4)
                .cache_mode(crate::CacheMode::Bypass)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("cancel-race resolve request must build"),
        )
        .expect("cancel-race resolve must submit");
    let handle = pending.handle();
    let mut wire = [0_u8; DNS_PACKET_LIMIT];
    silent
        .recv_from(&mut wire)
        .expect("configuration A must observe the cancel-race query");
    updates
        .send(Ok(ResolverConfig::snapshot_for_test(second.address)))
        .expect("cancel-race configuration B must publish");
    handle
        .cancel()
        .expect("cancel-race cancellation must submit");

    match pending.wait() {
        ResolveCompletion::Cancelled => {}
        ResolveCompletion::Failed(error) => {
            assert_eq!(error.kind(), ErrorKind::Transport);
            assert_eq!(error.dns_failure(), Some(DnsFailure::NoNameserver));
        }
        ResolveCompletion::Completed(response) => {
            panic!("cancel/configuration race unexpectedly completed: {response:?}")
        }
    }
    let terminal_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let metrics = engine.metrics();
        let terminals = metrics
            .resolutions_completed()
            .saturating_add(metrics.resolutions_failed())
            .saturating_add(metrics.resolutions_cancelled());
        if terminals == 1 && metrics.current().inflight_resolutions() == 0 {
            break;
        }
        assert!(
            Instant::now() < terminal_deadline,
            "cancel/configuration race did not release its DNS borrow exactly once"
        );
        thread::sleep(Duration::from_millis(5));
    }
    engine.shutdown().expect("cancel-race Engine must stop");
}

#[cfg(feature = "resolver")]
#[test]
fn route_generation_change_and_shutdown_commit_one_terminal_and_join() {
    use crate::{ResolveCompletion, ResolveRequest};

    let silent = StdUdpSocket::bind("127.0.0.1:0").expect("shutdown-race DNS fixture must bind");
    silent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("shutdown-race DNS read timeout");
    let first_address = silent.local_addr().expect("shutdown-race DNS address");
    let second = DnsFixture::answering(Ipv4Addr::LOCALHOST);
    let config = EngineConfig::spawned();
    let (factory, updates) =
        crate::backend::native_http::NativeHttpFactory::new_with_refreshable_nameserver(
            &config,
            first_address,
            Duration::from_millis(5),
        );
    let engine = crate::Engine::with_spawned_factory(config, Box::new(factory))
        .expect("shutdown-race Engine must construct");
    let shared = engine.shared_for_testing();
    let pending = engine
        .resolver()
        .submit(
            ResolveRequest::hostname("configuration-shutdown-race.test")
                .address_family(crate::AddressFamily::Ipv4)
                .cache_mode(crate::CacheMode::Bypass)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("shutdown-race resolve request must build"),
        )
        .expect("shutdown-race resolve must submit");
    let mut wire = [0_u8; DNS_PACKET_LIMIT];
    silent
        .recv_from(&mut wire)
        .expect("configuration A must observe the shutdown-race query");
    updates
        .send(Ok(ResolverConfig::snapshot_for_test(second.address)))
        .expect("shutdown-race configuration B must publish");
    engine.shutdown().expect("shutdown-race Engine must join");

    match pending.wait() {
        ResolveCompletion::Cancelled => {}
        ResolveCompletion::Failed(error) => assert!(
            error.kind() == ErrorKind::EngineStopped
                || (error.kind() == ErrorKind::Transport
                    && error.dns_failure() == Some(DnsFailure::NoNameserver)),
            "shutdown/configuration race returned an unrelated failure: {error:?}"
        ),
        other => panic!("shutdown/configuration race must terminate once: {other:?}"),
    }
    let metrics = shared.metrics_snapshot();
    assert_eq!(
        metrics
            .resolutions_completed()
            .saturating_add(metrics.resolutions_failed())
            .saturating_add(metrics.resolutions_cancelled()),
        1
    );
    assert_eq!(metrics.current().inflight_resolutions(), 0);
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
            let request =
                Message::from_vec(&buffer[..length]).expect("resolver pressure query must parse");
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
        .with_max_connections_per_origin(std::num::NonZeroUsize::new(1).expect("one is non-zero"))
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
    let factory =
        crate::backend::native_http::NativeHttpFactory::new_with_nameserver_and_verification_gate(
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
            if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
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
        let replacement_connection =
            ServerConnection::new(server_config).expect("TLS-error replacement state must build");
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
