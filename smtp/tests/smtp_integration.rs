#[path = "support/smtp_peer.rs"]
mod peer;

use std::net::SocketAddr;
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use nbreq::{Engine, EngineConfig, TcpConnectRequest, TlsOptions};
use nbreq_smtp::{
    Delivery, Envelope, FailureReason, RecipientPolicy, RecipientStatus, SendRequest, SmtpClient,
    SmtpServer, TlsPolicy,
};
use peer::{Entry, Gate, Identity, Peer, Step};

const MESSAGE: &[u8] = b"Subject: SMTP fixture\r\n\r\nfirst\r\n.second\r\nlast\r\n";
const WIRE_DATA: &[u8] = b"Subject: SMTP fixture\r\n\r\nfirst\r\n..second\r\nlast\r\n.\r\n";

fn engine(identity: &Identity) -> Engine {
    Engine::new(EngineConfig::spawned().with_additional_tls_root_certificate(identity.root_der.clone()))
        .expect("verified SMTP Engine")
}

fn request(address: SocketAddr, policy: TlsPolicy, recipients: &[&str]) -> SendRequest {
    let connect = TcpConnectRequest::literal(address)
        .connect_timeout(Duration::from_secs(2))
        .read_inactivity_timeout(Duration::from_secs(3))
        .write_inactivity_timeout(Duration::from_secs(3))
        .send_queue_bytes(64)
        .receive_queue_bytes(64)
        .build()
        .expect("fixture TCP request");
    let server = SmtpServer::new(
        connect,
        TlsOptions::new("127.0.0.1").expect("fixture TLS identity"),
        policy,
    );
    let envelope = Envelope::new(
        "sender@example.test",
        recipients.iter().map(|s| (*s).to_owned()).collect(),
    ).expect("fixture envelope");
    SendRequest::new(server, "client.example.test", envelope, MESSAGE.to_vec())
        .expect("fixture request")
        .command_timeout(Duration::from_secs(3))
        .overall_timeout(Duration::from_secs(7))
}

fn successful_exchange() -> Vec<Step> {
    vec![
        Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
        Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
        Step::Send(b"250 fixture\r\n".to_vec()),
        Step::ExpectLine(b"MAIL FROM:<sender@example.test>\r\n".to_vec()),
        Step::Send(b"250 sender ok\r\n".to_vec()),
        Step::ExpectLine(b"RCPT TO:<one@example.test>\r\n".to_vec()),
        Step::Send(b"250 recipient ok\r\n".to_vec()),
        Step::ExpectLine(b"DATA\r\n".to_vec()),
        Step::Send(b"354 send data\r\n".to_vec()),
        Step::ExpectBytes(WIRE_DATA.to_vec()),
        Step::Send(b"250 queued as fixture-1\r\n".to_vec()),
        Step::ExpectLine(b"QUIT\r\n".to_vec()),
        Step::Send(b"221 bye\r\n".to_vec()),
        Step::CloseNotify,
    ]
}

#[test]
fn implicit_tls_submits_exact_dot_stuffed_data_and_reports_final_250() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(&identity, Entry::ImplicitTls { expect_handshake: true }, successful_exchange());
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(peer.address, TlsPolicy::Implicit, &["one@example.test"]))
        .expect("SMTP operation starts");
    assert!(matches!(&outcome.delivery, Delivery::Accepted { reply } if reply.code == 250), "{outcome:?}");
    assert!(matches!(outcome.recipients[0].status, RecipientStatus::AcceptedForTransaction { .. }));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn required_starttls_reissues_ehlo_and_never_uses_pretls_capabilities() {
    let identity = Identity::localhost();
    let mut script = vec![
        Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
        Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
        Step::Send(b"250-fixture\r\n250-STARTTLS\r\n250 AUTH PLAIN\r\n".to_vec()),
        Step::ExpectLine(b"STARTTLS\r\n".to_vec()),
        Step::Send(b"220 ready for TLS\r\n".to_vec()),
        Step::Upgrade { expect_handshake: true },
    ];
    script.extend(successful_exchange().into_iter().skip(1));
    let peer = Peer::spawn(&identity, Entry::Plain, script);
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(peer.address, TlsPolicy::RequiredStartTls, &["one@example.test"]))
        .expect("SMTP operation starts");
    assert!(matches!(&outcome.delivery, Delivery::Accepted { reply } if reply.code == 250), "{outcome:?}");
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn required_starttls_absent_fails_before_mail_from() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(&identity, Entry::Plain, vec![
        Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
        Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
        Step::Send(b"250 fixture\r\n".to_vec()),
        Step::ExpectQuitOrClose,
    ]);
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(peer.address, TlsPolicy::RequiredStartTls, &["one@example.test"]))
        .expect("SMTP operation starts");
    assert!(matches!(outcome.delivery, Delivery::NotAccepted { reason: FailureReason::StartTlsUnavailable, .. }), "{outcome:?}");
    assert!(matches!(outcome.recipients[0].status, RecipientStatus::Unattempted));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn completed_data_terminator_without_final_reply_is_uncertain_and_not_retried() {
    let identity = Identity::localhost();
    let (gate, hold) = Gate::pair();
    let mut script = successful_exchange();
    script.truncate(10);
    script.push(hold);
    script.push(Step::Close);
    let peer = Peer::spawn(&identity, Entry::ImplicitTls { expect_handshake: true }, script);
    let engine = engine(&identity);
    let mut operation = SmtpClient::new(&engine)
        .submit(request(peer.address, TlsPolicy::Implicit, &["one@example.test"]))
        .expect("SMTP operation starts");
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        assert!(Instant::now() < deadline, "DATA terminator was not admitted");
        if gate.try_entered() { break; }
        assert!(matches!(operation.poll(), Poll::Pending), "operation completed before DATA reply");
        thread::sleep(Duration::from_millis(1));
    }
    gate.release();
    loop {
        assert!(Instant::now() < deadline, "SMTP operation did not terminalize");
        if let Poll::Ready(outcome) = operation.poll() {
            assert!(matches!(outcome.delivery, Delivery::Uncertain { .. }), "{outcome:?}");
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn rejected_recipient_prevents_data_under_default_policy() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(&identity, Entry::ImplicitTls { expect_handshake: true }, vec![
        Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
        Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
        Step::Send(b"250 fixture\r\n".to_vec()),
        Step::ExpectLine(b"MAIL FROM:<sender@example.test>\r\n".to_vec()),
        Step::Send(b"250 sender ok\r\n".to_vec()),
        Step::ExpectLine(b"RCPT TO:<one@example.test>\r\n".to_vec()),
        Step::Send(b"250 recipient ok\r\n".to_vec()),
        Step::ExpectLine(b"RCPT TO:<two@example.test>\r\n".to_vec()),
        Step::Send(b"550 no such recipient\r\n".to_vec()),
        Step::ExpectQuitOrClose,
    ]);
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(peer.address, TlsPolicy::Implicit, &["one@example.test", "two@example.test"]))
        .expect("SMTP operation starts");
    assert!(matches!(outcome.delivery, Delivery::NotAccepted { .. }), "{outcome:?}");
    assert!(matches!(outcome.recipients[0].status, RecipientStatus::AcceptedForTransaction { .. }));
    assert!(matches!(outcome.recipients[1].status, RecipientStatus::Rejected { .. }));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn opted_in_partial_recipients_submit_data_only_for_accepted_subset() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(&identity, Entry::ImplicitTls { expect_handshake: true }, vec![
        Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
        Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
        Step::Send(b"250 fixture\r\n".to_vec()),
        Step::ExpectLine(b"MAIL FROM:<sender@example.test>\r\n".to_vec()),
        Step::Send(b"250 sender ok\r\n".to_vec()),
        Step::ExpectLine(b"RCPT TO:<one@example.test>\r\n".to_vec()),
        Step::Send(b"250 recipient ok\r\n".to_vec()),
        Step::ExpectLine(b"RCPT TO:<two@example.test>\r\n".to_vec()),
        Step::Send(b"550 no such recipient\r\n".to_vec()),
        Step::ExpectLine(b"DATA\r\n".to_vec()),
        Step::Send(b"354 send data\r\n".to_vec()),
        Step::ExpectBytes(WIRE_DATA.to_vec()),
        Step::Send(b"250 accepted subset\r\n".to_vec()),
        Step::ExpectLine(b"QUIT\r\n".to_vec()),
        Step::Send(b"221 bye\r\n".to_vec()),
        Step::CloseNotify,
    ]);
    let engine = engine(&identity);
    let send_request = request(peer.address, TlsPolicy::Implicit, &["one@example.test", "two@example.test"])
        .recipient_policy(RecipientPolicy::AcceptedRecipients);
    let outcome = SmtpClient::new(&engine).send_blocking(send_request).expect("SMTP operation starts");
    assert!(matches!(outcome.delivery, Delivery::Accepted { .. }), "{outcome:?}");
    assert!(matches!(outcome.recipients[0].status, RecipientStatus::AcceptedForTransaction { .. }));
    assert!(matches!(outcome.recipients[1].status, RecipientStatus::Rejected { .. }));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}
