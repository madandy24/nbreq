#[path = "support/smtp_peer.rs"]
mod peer;

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use nbreq::{Engine, EngineConfig, TcpConnectRequest, TlsFailure, TlsOptions};
use nbreq_smtp::{
    Delivery, Envelope, FailureReason, RecipientPolicy, RecipientStatus, SendRequest, SmtpClient,
    SmtpErrorKind, SmtpServer, Stage, TlsPolicy,
};
use peer::{Entry, Gate, Identity, Peer, Step};

const MESSAGE: &[u8] = b"Subject: SMTP fixture\r\n\r\nfirst\r\n.second\r\nlast\r\n";
const WIRE_DATA: &[u8] = b"Subject: SMTP fixture\r\n\r\nfirst\r\n..second\r\nlast\r\n.\r\n";

fn engine(identity: &Identity) -> Engine {
    Engine::new(
        EngineConfig::spawned().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("verified SMTP Engine")
}

fn request(address: SocketAddr, policy: TlsPolicy, recipients: &[&str]) -> SendRequest {
    request_with(address, policy, recipients, MESSAGE.to_vec(), 64)
}

fn request_with(
    address: SocketAddr,
    policy: TlsPolicy,
    recipients: &[&str],
    message: Vec<u8>,
    window: usize,
) -> SendRequest {
    let connect = TcpConnectRequest::literal(address)
        .connect_timeout(Duration::from_secs(2))
        .read_inactivity_timeout(Duration::from_secs(3))
        .write_inactivity_timeout(Duration::from_secs(3))
        .send_queue_bytes(window)
        .receive_queue_bytes(window)
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
    )
    .expect("fixture envelope");
    SendRequest::new(server, "client.example.test", envelope, message)
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
        Step::ExpectTlsCloseNotify,
    ]
}

#[test]
fn implicit_tls_submits_exact_dot_stuffed_data_and_reports_final_250() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        successful_exchange(),
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(&outcome.delivery, Delivery::Accepted { reply } if reply.code == 250),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::AcceptedForTransaction { .. }
    ));
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
        Step::Upgrade {
            expect_handshake: true,
        },
    ];
    script.extend(successful_exchange().into_iter().skip(1));
    let peer = Peer::spawn(&identity, Entry::Plain, script);
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::RequiredStartTls,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(&outcome.delivery, Delivery::Accepted { reply } if reply.code == 250),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn required_starttls_absent_fails_before_mail_from() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::Plain,
        vec![
            Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
            Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
            Step::Send(b"250 fixture\r\n".to_vec()),
            Step::ExpectQuitOrClose,
        ],
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::RequiredStartTls,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                reason: FailureReason::StartTlsUnavailable,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::Unattempted
    ));
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
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = engine(&identity);
    let mut operation = SmtpClient::new(&engine)
        .submit(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        assert!(
            Instant::now() < deadline,
            "DATA terminator was not admitted"
        );
        if gate.try_entered() {
            break;
        }
        assert!(
            matches!(operation.poll(), Poll::Pending),
            "operation completed before DATA reply"
        );
        thread::sleep(Duration::from_millis(1));
    }
    gate.release();
    loop {
        assert!(
            Instant::now() < deadline,
            "SMTP operation did not terminalize"
        );
        if let Poll::Ready(outcome) = operation.poll() {
            assert!(
                matches!(outcome.delivery, Delivery::Uncertain { .. }),
                "{outcome:?}"
            );
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
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        vec![
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
        ],
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test", "two@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::NotAccepted { .. }),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::AcceptedForTransaction { .. }
    ));
    assert!(matches!(
        outcome.recipients[1].status,
        RecipientStatus::Rejected { .. }
    ));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn opted_in_partial_recipients_submit_data_only_for_accepted_subset() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        vec![
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
            Step::ExpectTlsCloseNotify,
        ],
    );
    let engine = engine(&identity);
    let send_request = request(
        peer.address,
        TlsPolicy::Implicit,
        &["one@example.test", "two@example.test"],
    )
    .recipient_policy(RecipientPolicy::AcceptedRecipients);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(send_request)
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::AcceptedForTransaction { .. }
    ));
    assert!(matches!(
        outcome.recipients[1].status,
        RecipientStatus::Rejected { .. }
    ));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn final_250_remains_accepted_after_quit_is_sent_and_operation_is_cancelled() {
    let identity = Identity::localhost();
    let (gate, hold) = Gate::pair();
    let mut script = successful_exchange();
    script.truncate(12); // Through observing QUIT, which proves the final DATA 250 was parsed.
    script.push(hold);
    script.push(Step::Close);
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = engine(&identity);
    let mut operation = SmtpClient::new(&engine)
        .submit(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        assert!(
            Instant::now() < deadline,
            "SMTP client never sent QUIT after final 250"
        );
        if gate.try_entered() {
            break;
        }
        let _ = operation.poll();
        thread::sleep(Duration::from_millis(1));
    }
    operation.cancel();
    let Poll::Ready(after_cancel) = operation.poll() else {
        panic!("cancel must terminalize");
    };
    assert!(
        matches!(after_cancel.delivery, Delivery::Accepted { .. }),
        "{after_cancel:?}"
    );
    let stable = after_cancel.clone();
    operation.cancel();
    let Poll::Ready(again) = operation.poll() else {
        panic!("terminal poll must remain ready");
    };
    assert_eq!(
        again, &stable,
        "cancel after terminal acceptance changed report"
    );
    gate.release();
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn cancel_with_rcpt_reply_outstanding_marks_current_unresolved_and_later_unattempted() {
    let identity = Identity::localhost();
    let (gate, hold) = Gate::pair();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        vec![
            Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
            Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
            Step::Send(b"250 fixture\r\n".to_vec()),
            Step::ExpectLine(b"MAIL FROM:<sender@example.test>\r\n".to_vec()),
            Step::Send(b"250 sender ok\r\n".to_vec()),
            Step::ExpectLine(b"RCPT TO:<one@example.test>\r\n".to_vec()),
            Step::Send(b"250 recipient ok\r\n".to_vec()),
            Step::ExpectLine(b"RCPT TO:<two@example.test>\r\n".to_vec()),
            hold,
            Step::ExpectClose,
        ],
    );
    let engine = engine(&identity);
    let mut operation = SmtpClient::new(&engine)
        .submit(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test", "two@example.test", "three@example.test"],
        ))
        .expect("SMTP operation starts");
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        assert!(Instant::now() < deadline, "second RCPT was not sent");
        if gate.try_entered() {
            break;
        }
        assert!(
            matches!(operation.poll(), Poll::Pending),
            "operation ended before RCPT reply"
        );
        thread::sleep(Duration::from_millis(1));
    }
    operation.cancel();
    let Poll::Ready(outcome) = operation.poll() else {
        panic!("cancel must terminalize");
    };
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                reason: FailureReason::Cancelled,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::AcceptedForTransaction { .. }
    ));
    assert!(matches!(
        outcome.recipients[1].status,
        RecipientStatus::Unresolved
    ));
    assert!(matches!(
        outcome.recipients[2].status,
        RecipientStatus::Unattempted
    ));
    let stable = outcome.clone();
    operation.cancel();
    assert!(matches!(operation.poll(), Poll::Ready(again) if again == &stable));
    gate.release();
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn bare_code_replies_and_rcpt_251_are_accepted() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        vec![
            Step::Send(b"220\r\n".to_vec()),
            Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
            Step::Send(b"250\r\n".to_vec()),
            Step::ExpectLine(b"MAIL FROM:<sender@example.test>\r\n".to_vec()),
            Step::Send(b"250\r\n".to_vec()),
            Step::ExpectLine(b"RCPT TO:<one@example.test>\r\n".to_vec()),
            Step::Send(b"251\r\n".to_vec()),
            Step::ExpectLine(b"DATA\r\n".to_vec()),
            Step::Send(b"354\r\n".to_vec()),
            Step::ExpectBytes(WIRE_DATA.to_vec()),
            Step::Send(b"250\r\n".to_vec()),
            Step::ExpectLine(b"QUIT\r\n".to_vec()),
            Step::Send(b"221\r\n".to_vec()),
            Step::ExpectTlsCloseNotify,
        ],
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    assert!(
        matches!(&outcome.recipients[0].status, RecipientStatus::AcceptedForTransaction { reply } if reply.code == 251)
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn malformed_and_unbounded_greetings_fail_without_sending_ehlo_or_mail() {
    let identity = Identity::localhost();
    let engine = engine(&identity);
    let mut too_many_lines = Vec::new();
    for _ in 0..33 {
        too_many_lines.extend_from_slice(b"220-more\r\n");
    }
    let mut too_many_bytes = Vec::new();
    for _ in 0..32 {
        too_many_bytes.extend_from_slice(b"220-");
        too_many_bytes.extend(vec![b'x'; 300]);
        too_many_bytes.extend_from_slice(b"\r\n");
    }
    let cases = [
        b"220-first\r\n250 last\r\n".to_vec(),
        b"220 bare LF\n".to_vec(),
        b"220 bare CR\rnext\r\n".to_vec(),
        {
            let mut line = b"220 ".to_vec();
            line.extend(vec![b'x'; 507]);
            line.extend(b"\r\n");
            line
        },
        too_many_lines,
        too_many_bytes,
    ];
    for greeting in cases {
        let peer = Peer::spawn(
            &identity,
            Entry::Plain,
            vec![Step::Send(greeting.clone()), Step::ExpectQuitOrClose],
        );
        let outcome = SmtpClient::new(&engine)
            .send_blocking(request(
                peer.address,
                TlsPolicy::RequiredStartTls,
                &["one@example.test"],
            ))
            .expect("SMTP operation starts");
        assert!(
            matches!(
                outcome.delivery,
                Delivery::NotAccepted {
                    reason: FailureReason::Protocol,
                    ..
                }
            ),
            "greeting {greeting:?}: {outcome:?}"
        );
        peer.join();
    }
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn one_byte_tcp_windows_dot_stuff_single_dot_and_append_missing_final_crlf() {
    let identity = Identity::localhost();
    let mut script = successful_exchange();
    script[9] = Step::ExpectBytes(b"Subject: tiny\r\n\r\n..\r\nend\r\n.\r\n".to_vec());
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = engine(&identity);
    let message = b"Subject: tiny\r\n\r\n.\r\nend".to_vec();
    let send_request = request_with(
        peer.address,
        TlsPolicy::Implicit,
        &["one@example.test"],
        message,
        1,
    );
    let outcome = SmtpClient::new(&engine)
        .send_blocking(send_request)
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn dot_at_four_kib_source_boundary_is_stuffed_once() {
    let identity = Identity::localhost();
    let mut message = b"Subject: boundary\r\n\r\n".to_vec();
    while message.len() + 1000 <= 4096 {
        message.extend(vec![b'x'; 998]);
        message.extend_from_slice(b"\r\n");
    }
    let remaining = 4096 - message.len();
    assert!(remaining >= 2);
    message.extend(vec![b'x'; remaining - 2]);
    message.extend_from_slice(b"\r\n");
    assert_eq!(message.len(), 4096);
    message.extend_from_slice(b".\r\nend\r\n");
    let mut expected = message[..4096].to_vec();
    expected.extend_from_slice(b"..\r\nend\r\n.\r\n");
    let mut script = successful_exchange();
    script[9] = Step::ExpectBytes(expected);
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = engine(&identity);
    let send_request = request_with(
        peer.address,
        TlsPolicy::Implicit,
        &["one@example.test"],
        message,
        64,
    );
    let outcome = SmtpClient::new(&engine)
        .send_blocking(send_request)
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn rcpt_421_stops_attempting_later_recipients_and_reports_prior_acceptance() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        vec![
            Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
            Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
            Step::Send(b"250 fixture\r\n".to_vec()),
            Step::ExpectLine(b"MAIL FROM:<sender@example.test>\r\n".to_vec()),
            Step::Send(b"250 sender ok\r\n".to_vec()),
            Step::ExpectLine(b"RCPT TO:<one@example.test>\r\n".to_vec()),
            Step::Send(b"250 recipient ok\r\n".to_vec()),
            Step::ExpectLine(b"RCPT TO:<two@example.test>\r\n".to_vec()),
            Step::Send(b"421 service closing\r\n".to_vec()),
            Step::ExpectQuitOrClose,
        ],
    );
    let engine = engine(&identity);
    let send_request = request(
        peer.address,
        TlsPolicy::Implicit,
        &["one@example.test", "two@example.test", "three@example.test"],
    )
    .recipient_policy(RecipientPolicy::AcceptedRecipients);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(send_request)
        .expect("SMTP operation starts");
    assert!(
        matches!(&outcome.delivery, Delivery::Rejected { stage: Stage::RcptTo(1), reply } if reply.code == 421),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::AcceptedForTransaction { .. }
    ));
    assert!(matches!(
        outcome.recipients[1].status,
        RecipientStatus::Rejected { .. }
    ));
    assert!(matches!(
        outcome.recipients[2].status,
        RecipientStatus::Unattempted
    ));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn starttls_surplus_plaintext_is_rejected_before_mail_from() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::Plain,
        vec![
            Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
            Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
            Step::Send(b"250-fixture\r\n250 STARTTLS\r\n".to_vec()),
            Step::ExpectLine(b"STARTTLS\r\n".to_vec()),
            Step::Send(b"220 ready for TLS\r\nSURPLUS".to_vec()),
            Step::ExpectQuitOrClose,
        ],
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::RequiredStartTls,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::NotAccepted { .. }),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::Unattempted
    ));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn high_bit_reply_byte_is_rejected_without_lossy_expansion() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::Plain,
        vec![
            Step::Send(b"220 invalid-\xff\r\n".to_vec()),
            Step::ExpectQuitOrClose,
        ],
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::RequiredStartTls,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                reason: FailureReason::Protocol,
                ..
            }
        ),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn explicit_final_data_rejection_is_known_and_malformed_final_is_uncertain() {
    let identity = Identity::localhost();
    let engine = engine(&identity);
    for (final_reply, rejected) in [
        (b"550 transaction rejected\r\n".to_vec(), true),
        (b"250 malformed bare LF\n".to_vec(), false),
    ] {
        let mut script = successful_exchange();
        script.truncate(10);
        script.push(Step::Send(final_reply));
        script.push(Step::ExpectQuitOrClose);
        let peer = Peer::spawn(
            &identity,
            Entry::ImplicitTls {
                expect_handshake: true,
            },
            script,
        );
        let outcome = SmtpClient::new(&engine)
            .send_blocking(request(
                peer.address,
                TlsPolicy::Implicit,
                &["one@example.test"],
            ))
            .expect("SMTP operation starts");
        if rejected {
            assert!(
                matches!(&outcome.delivery, Delivery::Rejected { stage: Stage::DataResult, reply } if reply.code == 550),
                "{outcome:?}"
            );
        } else {
            assert!(
                matches!(
                    outcome.delivery,
                    Delivery::Uncertain {
                        reason: FailureReason::Protocol,
                        ..
                    }
                ),
                "{outcome:?}"
            );
        }
        peer.join();
    }
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn manual_engine_needs_caller_drive_and_rejects_blocking_convenience() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        successful_exchange(),
    );
    let mut engine = Engine::new(
        EngineConfig::manual().with_additional_tls_root_certificate(identity.root_der.clone()),
    )
    .expect("manual SMTP Engine");
    let client = SmtpClient::new(&engine);
    let error = client
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect_err("blocking convenience must reject manual mode before network admission");
    assert_eq!(error.kind(), SmtpErrorKind::WrongMode);
    let mut operation = client
        .submit(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("manual submit");
    for _ in 0..3 {
        assert!(
            matches!(operation.poll(), Poll::Pending),
            "manual operation advanced without Engine drive"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            Instant::now() < deadline,
            "manual SMTP operation did not complete"
        );
        engine
            .drive(Instant::now() + Duration::from_millis(10))
            .expect("manual Engine drive");
        if let Poll::Ready(outcome) = operation.poll() {
            assert!(
                matches!(outcome.delivery, Delivery::Accepted { .. }),
                "{outcome:?}"
            );
            break;
        }
    }
    peer.join();
    engine.shutdown().expect("manual SMTP Engine shutdown");
}

#[test]
fn implicit_tls_wrong_name_reports_structured_verification_failure() {
    let identity = Identity::for_host("other.example.test");
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: false,
        },
        vec![],
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                stage: Stage::Connect,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.transport_diagnostic.and_then(|d| d.tls_failure),
        Some(TlsFailure::CertificateHostnameMismatch)
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::Unattempted
    ));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn implicit_tls_untrusted_root_reports_structured_verification_failure() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: false,
        },
        vec![],
    );
    let engine = Engine::new(EngineConfig::spawned()).expect("default-trust Engine");
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                stage: Stage::Connect,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.transport_diagnostic.and_then(|d| d.tls_failure),
        Some(TlsFailure::CertificateUnknownIssuer)
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::Unattempted
    ));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn starttls_wrong_name_never_sends_mail_after_failed_upgrade() {
    let identity = Identity::for_host("other.example.test");
    let peer = Peer::spawn(
        &identity,
        Entry::Plain,
        vec![
            Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
            Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
            Step::Send(b"250-fixture\r\n250 STARTTLS\r\n".to_vec()),
            Step::ExpectLine(b"STARTTLS\r\n".to_vec()),
            Step::Send(b"220 ready for TLS\r\n".to_vec()),
            Step::Upgrade {
                expect_handshake: false,
            },
        ],
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::RequiredStartTls,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                stage: Stage::Upgrade,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.transport_diagnostic.and_then(|d| d.tls_failure),
        Some(TlsFailure::CertificateHostnameMismatch)
    );
    assert!(matches!(
        outcome.recipients[0].status,
        RecipientStatus::Unattempted
    ));
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn reply_text_may_contain_horizontal_tab() {
    let identity = Identity::localhost();
    let mut script = successful_exchange();
    script[2] = Step::Send(b"250 ok\tqueued\r\n".to_vec());
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn valid_359_data_challenge_and_259_final_completion_are_accepted() {
    let identity = Identity::localhost();
    let mut script = successful_exchange();
    script[8] = Step::Send(b"359 send data\r\n".to_vec());
    script[10] = Step::Send(b"259 accepted\r\n".to_vec());
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(&outcome.delivery, Delivery::Accepted { reply } if reply.code == 259),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn valid_259_final_completion_is_accepted() {
    let identity = Identity::localhost();
    let mut script = successful_exchange();
    script[10] = Step::Send(b"259 accepted\r\n".to_vec());
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = engine(&identity);
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("SMTP operation starts");
    assert!(
        matches!(&outcome.delivery, Delivery::Accepted { reply } if reply.code == 259),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn malformed_or_wrong_class_final_data_reply_is_uncertain() {
    let identity = Identity::localhost();
    let engine = engine(&identity);
    for code in [
        b"299\r\n".to_vec(),
        b"600\r\n".to_vec(),
        b"350\r\n".to_vec(),
    ] {
        let mut script = successful_exchange();
        script.truncate(10);
        script.push(Step::Send(code.clone()));
        script.push(Step::ExpectQuitOrClose);
        let peer = Peer::spawn(
            &identity,
            Entry::ImplicitTls {
                expect_handshake: true,
            },
            script,
        );
        let outcome = SmtpClient::new(&engine)
            .send_blocking(request(
                peer.address,
                TlsPolicy::Implicit,
                &["one@example.test"],
            ))
            .expect("SMTP operation starts");
        assert!(
            matches!(
                outcome.delivery,
                Delivery::Uncertain {
                    reason: FailureReason::Protocol,
                    ..
                }
            ),
            "reply {code:?}: {outcome:?}"
        );
        peer.join();
    }
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn fragmented_trickled_greeting_still_hits_absolute_command_deadline() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::Plain,
        vec![
            Step::Send(b"2".to_vec()),
            Step::Delay(Duration::from_millis(100)),
            Step::Send(b"2".to_vec()),
            Step::Delay(Duration::from_millis(100)),
            Step::Send(b"0".to_vec()),
            Step::Delay(Duration::from_millis(400)),
            Step::ExpectClose,
        ],
    );
    let engine = engine(&identity);
    let request = request(
        peer.address,
        TlsPolicy::RequiredStartTls,
        &["one@example.test"],
    )
    .command_timeout(Duration::from_millis(300))
    .overall_timeout(Duration::from_secs(3));
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request)
        .expect("SMTP operation starts");
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                stage: Stage::Greeting,
                reason: FailureReason::Timeout
            }
        ),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn underlying_tcp_read_timeout_is_reported_as_timeout_not_generic_transport() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::Plain,
        vec![Step::Delay(Duration::from_millis(500)), Step::ExpectClose],
    );
    let engine = engine(&identity);
    let connect = TcpConnectRequest::literal(peer.address)
        .connect_timeout(Duration::from_secs(2))
        .read_inactivity_timeout(Duration::from_millis(150))
        .write_inactivity_timeout(Duration::from_secs(2))
        .send_queue_bytes(64)
        .receive_queue_bytes(64)
        .build()
        .expect("fixture TCP request");
    let server = SmtpServer::new(
        connect,
        TlsOptions::new("127.0.0.1").expect("identity"),
        TlsPolicy::RequiredStartTls,
    );
    let envelope =
        Envelope::new("sender@example.test", vec!["one@example.test".into()]).expect("envelope");
    let request = SendRequest::new(server, "client.example.test", envelope, MESSAGE.to_vec())
        .expect("request")
        .command_timeout(Duration::from_secs(2))
        .overall_timeout(Duration::from_secs(4));
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request)
        .expect("SMTP operation starts");
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                stage: Stage::Greeting,
                reason: FailureReason::Timeout
            }
        ),
        "{outcome:?}"
    );
    assert!(
        outcome
            .transport_diagnostic
            .and_then(|d| d.timeout_kind)
            .is_some(),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn dropping_pending_manual_connect_releases_one_slot_and_replacement_succeeds() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        successful_exchange(),
    );
    let mut engine = Engine::new(
        EngineConfig::manual()
            .with_additional_tls_root_certificate(identity.root_der.clone())
            .with_max_standalone_tcp_connections(NonZeroUsize::new(1).expect("one slot"))
            .with_max_queued_bytes(64 + 64 + 256 * 1024),
    )
    .expect("one-slot manual Engine");
    let client = SmtpClient::new(&engine);
    let mut abandoned = client
        .submit(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("first submit");
    assert!(matches!(abandoned.poll(), Poll::Pending));
    drop(abandoned);
    for _ in 0..5 {
        engine
            .drive(Instant::now() + Duration::from_millis(10))
            .expect("drain cancellation");
    }
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 0);
    assert_eq!(engine.metrics().current().reserved_tcp_queue_bytes(), 0);

    let mut replacement = client
        .submit(request(
            peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("replacement submit");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(Instant::now() < deadline, "replacement did not complete");
        engine
            .drive(Instant::now() + Duration::from_millis(10))
            .expect("manual drive");
        if let Poll::Ready(outcome) = replacement.poll() {
            assert!(
                matches!(outcome.delivery, Delivery::Accepted { .. }),
                "{outcome:?}"
            );
            break;
        }
    }
    peer.join();
    engine.shutdown().expect("manual Engine shutdown");
}

#[test]
fn cancellation_of_live_smtp_connection_reclaims_slot_for_replacement() {
    let identity = Identity::localhost();
    let (gate, hold) = Gate::pair();
    let blocked = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        vec![
            Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
            Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
            hold,
            Step::ExpectClose,
        ],
    );
    let engine = Engine::new(
        EngineConfig::spawned()
            .with_additional_tls_root_certificate(identity.root_der.clone())
            .with_max_standalone_tcp_connections(NonZeroUsize::new(1).expect("one slot"))
            .with_max_queued_bytes(64 + 64 + 256 * 1024),
    )
    .expect("one-slot spawned Engine");
    let client = SmtpClient::new(&engine);
    let mut operation = client
        .submit(request(
            blocked.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("blocked submit");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            Instant::now() < deadline,
            "blocked operation did not reach EHLO"
        );
        if gate.try_entered() {
            break;
        }
        assert!(matches!(operation.poll(), Poll::Pending));
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(engine.metrics().current().standalone_tcp_connections(), 1);
    operation.cancel();
    gate.release();
    blocked.join();
    loop {
        assert!(
            Instant::now() < deadline,
            "cancelled connection slot was not released"
        );
        let current = engine.metrics().current();
        if current.standalone_tcp_connections() == 0 && current.reserved_tcp_queue_bytes() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    let replacement = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        successful_exchange(),
    );
    let outcome = client
        .send_blocking(request(
            replacement.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("replacement send starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    replacement.join();
    engine.shutdown().expect("spawned Engine shutdown");
}

#[test]
fn overall_deadline_ends_transaction_even_when_command_deadline_is_longer() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::Plain,
        vec![
            Step::Send(b"220 fixture ESMTP\r\n".to_vec()),
            Step::ExpectLine(b"EHLO client.example.test\r\n".to_vec()),
            Step::Send(b"250-fixture\r\n250 STARTTLS\r\n".to_vec()),
            Step::ExpectLine(b"STARTTLS\r\n".to_vec()),
            Step::Delay(Duration::from_secs(3)),
            Step::ExpectClose,
        ],
    );
    let engine = engine(&identity);
    let request = request(
        peer.address,
        TlsPolicy::RequiredStartTls,
        &["one@example.test"],
    )
    .command_timeout(Duration::from_secs(5))
    .overall_timeout(Duration::from_secs(2));
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request)
        .expect("SMTP operation starts");
    assert!(
        matches!(
            outcome.delivery,
            Delivery::NotAccepted {
                stage: Stage::StartTls,
                reason: FailureReason::Timeout
            }
        ),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("SMTP Engine shutdown");
}

#[test]
fn stalled_submission_does_not_prevent_a_second_submission_from_completing() {
    let identity = Identity::localhost();
    let (gate, hold) = Gate::pair();
    let stalled = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        vec![hold, Step::ExpectClose],
    );
    let engine = Engine::new(
        EngineConfig::spawned()
            .with_additional_tls_root_certificate(identity.root_der.clone())
            .with_max_standalone_tcp_connections(NonZeroUsize::new(2).expect("two slots"))
            .with_max_queued_bytes(2 * (64 + 64 + 256 * 1024)),
    )
    .expect("two-slot Engine");
    let client = SmtpClient::new(&engine);
    let mut stalled_operation = client
        .submit(request(
            stalled.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("stalled submit");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            Instant::now() < deadline,
            "stalled TLS session did not reach gate"
        );
        if gate.try_entered() {
            break;
        }
        assert!(matches!(stalled_operation.poll(), Poll::Pending));
        thread::sleep(Duration::from_millis(1));
    }
    let healthy = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        successful_exchange(),
    );
    let outcome = client
        .send_blocking(request(
            healthy.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("second send starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    healthy.join();
    stalled_operation.cancel();
    gate.release();
    stalled.join();
    engine.shutdown().expect("Engine shutdown");
}

#[test]
fn dense_dot_lines_cross_many_staging_chunks_without_losing_or_adding_bytes() {
    let identity = Identity::localhost();
    let mut message = b"Subject: dense dots\r\n\r\n".to_vec();
    let mut expected = message.clone();
    for _ in 0..128 {
        message.push(b'.');
        message.extend(vec![b'x'; 996]);
        message.extend_from_slice(b"\r\n");
        expected.extend_from_slice(b"..");
        expected.extend(vec![b'x'; 996]);
        expected.extend_from_slice(b"\r\n");
    }
    expected.extend_from_slice(b".\r\n");
    let mut script = successful_exchange();
    script[9] = Step::ExpectBytes(expected);
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = engine(&identity);
    let request = request_with(
        peer.address,
        TlsPolicy::Implicit,
        &["one@example.test"],
        message,
        512,
    );
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request)
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("Engine shutdown");
}

#[test]
fn omitted_request_windows_use_small_engine_default_without_fixed_staging_assumptions() {
    let identity = Identity::localhost();
    let peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        successful_exchange(),
    );
    let engine = Engine::new(
        EngineConfig::spawned()
            .with_additional_tls_root_certificate(identity.root_der.clone())
            .with_max_tcp_queue_bytes_per_connection(37)
            .with_max_queued_bytes(37 + 37 + 256 * 1024),
    )
    .expect("small-default-window Engine");
    let connect = TcpConnectRequest::literal(peer.address)
        .connect_timeout(Duration::from_secs(2))
        .read_inactivity_timeout(Duration::from_secs(3))
        .write_inactivity_timeout(Duration::from_secs(3))
        .build()
        .expect("request with omitted queue windows");
    assert_eq!(connect.send_queue_bytes(), None);
    assert_eq!(connect.receive_queue_bytes(), None);
    let server = SmtpServer::new(
        connect,
        TlsOptions::new("127.0.0.1").expect("TLS identity"),
        TlsPolicy::Implicit,
    );
    let envelope =
        Envelope::new("sender@example.test", vec!["one@example.test".into()]).expect("envelope");
    let request = SendRequest::new(server, "client.example.test", envelope, MESSAGE.to_vec())
        .expect("request");
    let outcome = SmtpClient::new(&engine)
        .send_blocking(request)
        .expect("SMTP operation starts");
    assert!(
        matches!(outcome.delivery, Delivery::Accepted { .. }),
        "{outcome:?}"
    );
    peer.join();
    engine.shutdown().expect("Engine shutdown");
}

#[test]
fn actively_polled_large_data_stall_allows_small_submission_then_reclaims_resources() {
    let identity = Identity::localhost();
    let mut large_message = Vec::with_capacity(4 * 1024 * 1024);
    large_message.extend_from_slice(b"Subject: large fixture\r\n\r\n");
    for _ in 0..3200 {
        large_message.extend(vec![b'x'; 998]);
        large_message.extend_from_slice(b"\r\n");
    }
    assert!(large_message.len() > 3 * 1024 * 1024);
    let (gate, hold) = Gate::pair();
    let mut script = successful_exchange();
    script.truncate(9); // Through the DATA 354 reply.
    script.push(Step::ExpectBytes(large_message[..16 * 1024].to_vec()));
    script.push(hold);
    script.push(Step::Close);
    let large_peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        script,
    );
    let engine = Engine::new(
        EngineConfig::spawned()
            .with_additional_tls_root_certificate(identity.root_der.clone())
            .with_max_standalone_tcp_connections(NonZeroUsize::new(2).expect("two slots"))
            .with_max_queued_bytes(2 * (64 + 64 + 256 * 1024)),
    )
    .expect("two-slot Engine");
    let client = SmtpClient::new(&engine);
    let large_request = request_with(
        large_peer.address,
        TlsPolicy::Implicit,
        &["one@example.test"],
        large_message,
        64,
    );
    let mut large = client.submit(large_request).expect("large submit");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            Instant::now() < deadline,
            "large DATA did not reach peer gate"
        );
        if gate.try_entered() {
            break;
        }
        assert!(
            matches!(large.poll(), Poll::Pending),
            "large operation ended before DATA gate"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let small_peer = Peer::spawn(
        &identity,
        Entry::ImplicitTls {
            expect_handshake: true,
        },
        successful_exchange(),
    );
    let mut small = client
        .submit(request(
            small_peer.address,
            TlsPolicy::Implicit,
            &["one@example.test"],
        ))
        .expect("small submit");
    loop {
        assert!(
            Instant::now() < deadline,
            "small submission was starved by large DATA poll"
        );
        assert!(
            matches!(large.poll(), Poll::Pending),
            "large DATA operation ended while the small submission was in flight"
        );
        if let Poll::Ready(outcome) = small.poll() {
            assert!(
                matches!(outcome.delivery, Delivery::Accepted { .. }),
                "{outcome:?}"
            );
            assert!(
                matches!(large.poll(), Poll::Pending),
                "large DATA operation ended before cancellation"
            );
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    small_peer.join();
    large.cancel();
    let Poll::Ready(cancelled) = large.poll() else {
        panic!("large cancellation did not terminalize");
    };
    assert!(
        matches!(
            cancelled.delivery,
            Delivery::NotAccepted { .. } | Delivery::Uncertain { .. }
        ),
        "{cancelled:?}"
    );
    gate.release();
    large_peer.join();
    loop {
        assert!(
            Instant::now() < deadline,
            "large cancellation did not release resources"
        );
        let current = engine.metrics().current();
        if current.standalone_tcp_connections() == 0 && current.reserved_tcp_queue_bytes() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    engine.shutdown().expect("Engine shutdown");
}
