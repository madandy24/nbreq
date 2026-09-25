use std::time::Duration;

use nbreq::{Engine, EngineConfig, TcpConnectRequest, TlsOptions};
use nbreq_smtp::{Envelope, SendRequest, SmtpClient, SmtpErrorKind, SmtpServer, TlsPolicy};

const GOOD: &[u8] = b"From: sender@example.test\r\nSubject: fixture\r\n\r\nbody\r\n";

fn server() -> SmtpServer {
    let connect = TcpConnectRequest::literal("127.0.0.1:1".parse().expect("literal"))
        .build()
        .expect("TCP request");
    SmtpServer::new(
        connect,
        TlsOptions::new("localhost").expect("TLS identity"),
        TlsPolicy::Implicit,
    )
}

fn envelope() -> Envelope {
    Envelope::new("sender@example.test", vec!["recipient@example.test".into()])
        .expect("valid envelope")
}

fn request(message: &[u8]) -> Result<SendRequest, nbreq_smtp::SmtpError> {
    SendRequest::new(
        server(),
        "client.example.test",
        envelope(),
        message.to_vec(),
    )
}

#[test]
fn envelope_rejects_injection_invalid_mailboxes_and_recipient_count() {
    for from in [
        "a\r\nRCPT TO:<bad@example.test>",
        "a\nb@example.test",
        "a@example.test>",
        "ü@example.test",
    ] {
        assert!(
            Envelope::new(from, vec!["recipient@example.test".into()]).is_err(),
            "sender {from:?}"
        );
    }
    for recipient in [
        "a\r\nDATA",
        "<a@example.test>",
        "a b@example.test",
        "ü@example.test",
    ] {
        assert!(
            Envelope::new("sender@example.test", vec![recipient.into()]).is_err(),
            "recipient {recipient:?}"
        );
    }
    assert!(Envelope::new("sender@example.test", Vec::new()).is_err());
    assert!(Envelope::new("sender@example.test", vec!["r@example.test".into(); 101]).is_err());
    assert!(
        Envelope::new("", vec!["r@example.test".into()]).is_ok(),
        "null reverse path is explicit"
    );
}

#[test]
fn request_rejects_unsafe_ehlo_and_unprepared_message() {
    for ehlo in [
        "client\r\nMAIL FROM:<bad@example.test>",
        "client\nnext",
        "",
        "ü.example",
    ] {
        assert!(
            SendRequest::new(server(), ehlo, envelope(), GOOD.to_vec()).is_err(),
            "EHLO {ehlo:?}"
        );
    }
    for message in [
        &b"Subject: x\n\nbody"[..],
        &b"Subject: x\rbody\r\n"[..],
        &b"Subject: x\r\nbody\r\n"[..],
        &b"\r\nbody\r\n"[..],
        &b"Subject: x\r\n\r\nbad\0byte\r\n"[..],
        &b"Subject: x\r\n\r\nbad\x7fbyte\r\n"[..],
        &b"Subject: bad\x7f\r\n\r\nbody\r\n"[..],
        &b"Subject: x\r\n\r\nbad\x01byte\r\n"[..],
        &b"Subject: x\r\n\r\nnonascii-\x80\r\n"[..],
    ] {
        assert!(request(message).is_err(), "message {message:?}");
    }
    let mut overlong = b"Subject: x\r\n\r\n".to_vec();
    overlong.extend(vec![b'x'; 999]);
    overlong.extend(b"\r\n");
    assert!(
        request(&overlong).is_err(),
        "content line exceeds 998 bytes"
    );
    let mut oversize = b"Subject: x\r\n\r\n".to_vec();
    oversize.extend(vec![b'x'; 8 * 1024 * 1024]);
    assert!(
        request(&oversize).is_err(),
        "prepared message exceeds 8 MiB"
    );
}

#[test]
fn finite_deadlines_are_checked_before_network_admission() {
    let engine = Engine::new(EngineConfig::spawned()).expect("Engine");
    let client = SmtpClient::new(&engine);
    let zero_command = request(GOOD)
        .expect("request")
        .command_timeout(Duration::ZERO);
    assert_eq!(
        client
            .submit(zero_command)
            .err()
            .expect("zero timeout error")
            .kind(),
        SmtpErrorKind::InvalidRequest
    );
    let zero_overall = request(GOOD)
        .expect("request")
        .overall_timeout(Duration::ZERO);
    assert_eq!(
        client
            .submit(zero_overall)
            .err()
            .expect("zero timeout error")
            .kind(),
        SmtpErrorKind::InvalidRequest
    );
    engine.shutdown().expect("Engine shutdown");
}

#[test]
fn request_and_envelope_debug_do_not_reveal_private_input() {
    let secret = "private-sentinel@example.test";
    let envelope = Envelope::new(secret, vec!["recipient@example.test".into()]).expect("envelope");
    assert!(
        !format!("{envelope:?}").contains(secret),
        "envelope Debug leaked sender"
    );
    let message = b"Subject: private-body-sentinel\r\n\r\nbody\r\n".to_vec();
    let request =
        SendRequest::new(server(), "client.example.test", envelope, message).expect("request");
    let debug = format!("{request:?}");
    assert!(!debug.contains(secret), "request Debug leaked sender");
    assert!(
        !debug.contains("private-body-sentinel"),
        "request Debug leaked body"
    );
}

#[test]
fn ehlo_ipv6_address_literal_uses_smtp_ipv6_tag() {
    assert!(SendRequest::new(server(), "[IPv6:::1]", envelope(), GOOD.to_vec()).is_ok());
    assert!(SendRequest::new(server(), "[ipv6:::1]", envelope(), GOOD.to_vec()).is_ok());
    assert!(SendRequest::new(server(), "[::1]", envelope(), GOOD.to_vec()).is_err());
}

#[test]
fn invalid_tls_handshake_deadline_fails_before_tcp_admission_for_both_policies() {
    let engine = Engine::new(EngineConfig::spawned()).expect("Engine");
    let client = SmtpClient::new(&engine);
    for policy in [TlsPolicy::Implicit, TlsPolicy::RequiredStartTls] {
        for timeout in [Duration::ZERO, Duration::MAX] {
            let connect = TcpConnectRequest::literal("127.0.0.1:1".parse().expect("literal"))
                .build()
                .expect("TCP request");
            let tls = TlsOptions::new("localhost")
                .expect("TLS identity")
                .handshake_timeout(timeout);
            let server = SmtpServer::new(connect, tls, policy);
            let request =
                SendRequest::new(server, "client.example.test", envelope(), GOOD.to_vec())
                    .expect("request");
            let before = engine.metrics().tcp_connects_accepted();
            let error = client
                .submit(request)
                .err()
                .expect("invalid TLS timeout must reject submission");
            assert_eq!(error.kind(), SmtpErrorKind::InvalidRequest);
            assert_eq!(
                engine.metrics().tcp_connects_accepted(),
                before,
                "invalid TLS policy must not dial"
            );
        }
    }
    engine.shutdown().expect("Engine shutdown");
}

#[test]
fn prepared_message_capacity_and_valid_near_limit_framing_are_bounded() {
    let mut overallocated = Vec::with_capacity(8 * 1024 * 1024 + 1);
    overallocated.extend_from_slice(GOOD);
    assert!(
        SendRequest::new(server(), "client.example.test", envelope(), overallocated).is_err(),
        "retained capacity above 8 MiB must be rejected even for a short message"
    );

    const LIMIT: usize = 8 * 1024 * 1024;
    let mut message = Vec::with_capacity(LIMIT);
    message.extend_from_slice(b"Subject: near-limit\r\n\r\n");
    while message.len() + 1000 <= LIMIT {
        message.extend(vec![b'x'; 998]);
        message.extend_from_slice(b"\r\n");
    }
    let remainder = LIMIT - message.len();
    assert!(remainder >= 2);
    message.extend(vec![b'x'; remainder - 2]);
    message.extend_from_slice(b"\r\n");
    assert_eq!(message.len(), LIMIT);
    assert!(
        SendRequest::new(server(), "client.example.test", envelope(), message.clone()).is_ok(),
        "fully framed 8 MiB message should admit"
    );
    message.extend_from_slice(b"x\r\n");
    assert!(
        SendRequest::new(server(), "client.example.test", envelope(), message).is_err(),
        "valid line beyond 8 MiB must reject on size rather than line syntax"
    );
}
