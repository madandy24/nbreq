use std::error::Error as StdError;
use std::fmt;
use std::time::Duration;
use std::time::Instant;

use nbreq::{TcpConnectRequest, TlsOptions};

use crate::client::TransportDiagnostic;

pub(crate) const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_RECIPIENTS: usize = 100;

/// Required transport security for one SMTP connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsPolicy {
    /// Start TLS immediately after TCP connection establishment.
    Implicit,
    /// Negotiate STARTTLS and require a verified upgrade before any mail command.
    RequiredStartTls,
}

/// Connection destination and certificate identity for an SMTP server.
#[derive(Clone, Debug)]
pub struct SmtpServer {
    pub(crate) connect: TcpConnectRequest,
    pub(crate) tls: TlsOptions,
    pub(crate) policy: TlsPolicy,
}

impl SmtpServer {
    /// Combines an NBReq TCP request with its separately verified TLS identity.
    #[must_use]
    pub fn new(connect: TcpConnectRequest, tls: TlsOptions, policy: TlsPolicy) -> Self {
        Self {
            connect,
            tls,
            policy,
        }
    }
}

/// Validated SMTP reverse path and forward paths.
#[derive(Clone)]
pub struct Envelope {
    pub(crate) from: String,
    pub(crate) recipients: Vec<String>,
}

impl fmt::Debug for Envelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Envelope")
            .field("null_sender", &self.from.is_empty())
            .field("recipient_count", &self.recipients.len())
            .finish()
    }
}

impl Envelope {
    /// Builds an envelope. An empty sender denotes the null reverse path (`<>`).
    pub fn new(from: impl Into<String>, recipients: Vec<String>) -> Result<Self, SmtpError> {
        let from = from.into();
        if !from.is_empty() {
            validate_mailbox(&from)?;
        }
        if recipients.is_empty() || recipients.len() > MAX_RECIPIENTS {
            return Err(SmtpError::invalid("SMTP requires 1 to 100 recipients"));
        }
        let recipients = recipients
            .iter()
            .map(|address| {
                validate_mailbox(address)?;
                Ok(address.as_str().to_owned())
            })
            .collect::<Result<Vec<_>, SmtpError>>()?;
        Ok(Self {
            from: from.as_str().to_owned(),
            recipients,
        })
    }

    /// Returns the reverse-path mailbox, or an empty string for the null path.
    #[must_use]
    pub fn from(&self) -> &str {
        &self.from
    }

    /// Returns all requested forward-path mailboxes in command order.
    #[must_use]
    pub fn recipients(&self) -> &[String] {
        &self.recipients
    }
}

/// Policy for a transaction with rejected recipients.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecipientPolicy {
    /// Do not send DATA if any RCPT is rejected. This is the default.
    RequireAllRecipients,
    /// Send DATA to recipients the server accepted, if at least one was accepted.
    AcceptedRecipients,
}

/// Owned single-message SMTP submission request.
pub struct SendRequest {
    pub(crate) server: SmtpServer,
    pub(crate) ehlo_name: String,
    pub(crate) envelope: Envelope,
    pub(crate) message: Vec<u8>,
    pub(crate) recipient_policy: RecipientPolicy,
    pub(crate) command_timeout: Duration,
    pub(crate) overall_timeout: Duration,
}

impl fmt::Debug for SendRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SendRequest")
            .field("server", &self.server)
            .field("ehlo_name_len", &self.ehlo_name.len())
            .field("envelope", &self.envelope)
            .field("message_len", &self.message.len())
            .field("message_capacity", &self.message.capacity())
            .field("recipient_policy", &self.recipient_policy)
            .field("command_timeout", &self.command_timeout)
            .field("overall_timeout", &self.overall_timeout)
            .finish()
    }
}

impl SendRequest {
    /// Creates a request from a prepared seven-bit message with canonical CRLF lines.
    pub fn new(
        server: SmtpServer,
        ehlo_name: impl Into<String>,
        envelope: Envelope,
        message: Vec<u8>,
    ) -> Result<Self, SmtpError> {
        let ehlo_name = ehlo_name.into();
        validate_ehlo(&ehlo_name)?;
        validate_message(&message)?;
        Ok(Self {
            server,
            ehlo_name: ehlo_name.as_str().to_owned(),
            envelope,
            message,
            recipient_policy: RecipientPolicy::RequireAllRecipients,
            command_timeout: Duration::from_secs(30),
            overall_timeout: Duration::from_secs(120),
        })
    }

    /// Selects whether partial recipient acceptance may lead to DATA.
    #[must_use]
    pub fn recipient_policy(mut self, policy: RecipientPolicy) -> Self {
        self.recipient_policy = policy;
        self
    }

    /// Sets the finite deadline for each SMTP command or protocol phase.
    #[must_use]
    pub fn command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Sets the finite ceiling for the entire submission.
    #[must_use]
    pub fn overall_timeout(mut self, timeout: Duration) -> Self {
        self.overall_timeout = timeout;
        self
    }

    pub(crate) fn validate_deadlines(&self) -> Result<(), SmtpError> {
        let now = Instant::now();
        if self.command_timeout.is_zero()
            || self.overall_timeout.is_zero()
            || self.server.tls.timeout().is_zero()
            || now.checked_add(self.command_timeout).is_none()
            || now.checked_add(self.overall_timeout).is_none()
            || now.checked_add(self.server.tls.timeout()).is_none()
        {
            return Err(SmtpError::invalid(
                "SMTP and TLS timeouts must be finite, positive, and representable",
            ));
        }
        Ok(())
    }
}

fn validate_mailbox(address: &str) -> Result<(), SmtpError> {
    if address.is_empty() || address.len() > 254 || !address.is_ascii() {
        return Err(SmtpError::invalid(
            "invalid SMTP mailbox length or encoding",
        ));
    }
    let Some((local, domain)) = address.rsplit_once('@') else {
        return Err(SmtpError::invalid("SMTP mailbox must contain one @"));
    };
    if local.is_empty() || local.len() > 64 || local.starts_with('.') || local.ends_with('.') {
        return Err(SmtpError::invalid("unsupported SMTP local part"));
    }
    let mut previous_dot = false;
    for byte in local.bytes() {
        if byte == b'.' {
            if previous_dot {
                return Err(SmtpError::invalid("unsupported SMTP local part"));
            }
            previous_dot = true;
        } else {
            previous_dot = false;
            if !(byte.is_ascii_alphanumeric() || b"!#$%&'*+/=?^_`{|}~-".contains(&byte)) {
                return Err(SmtpError::invalid("unsupported SMTP local part"));
            }
        }
    }
    validate_domain(domain)
}

fn validate_domain(domain: &str) -> Result<(), SmtpError> {
    if domain.is_empty() || domain.len() > 253 || !domain.is_ascii() {
        return Err(SmtpError::invalid("invalid SMTP domain"));
    }
    for label in domain.split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label.as_bytes()[0].is_ascii_alphanumeric()
            || !label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(SmtpError::invalid("invalid SMTP domain label"));
        }
    }
    Ok(())
}

fn validate_ehlo(name: &str) -> Result<(), SmtpError> {
    if name.len() > 255 {
        return Err(SmtpError::invalid("SMTP EHLO identity is too long"));
    }
    if let Some(literal) = name
        .strip_prefix('[')
        .and_then(|part| part.strip_suffix(']'))
    {
        if literal.parse::<std::net::Ipv4Addr>().is_ok() {
            return Ok(());
        }
        if literal
            .get(..5)
            .is_some_and(|tag| tag.eq_ignore_ascii_case("IPv6:"))
            && literal
                .get(5..)
                .is_some_and(|ip| ip.parse::<std::net::Ipv6Addr>().is_ok())
        {
            return Ok(());
        }
    }
    validate_domain(name)
}

fn validate_message(message: &Vec<u8>) -> Result<(), SmtpError> {
    let suffix = if message.ends_with(b"\r\n") { 0 } else { 2 };
    if message.capacity() > MAX_MESSAGE_BYTES
        || message.len().saturating_add(suffix) > MAX_MESSAGE_BYTES
    {
        return Err(SmtpError::invalid(
            "prepared SMTP message exceeds 8 MiB allocation or length",
        ));
    }
    if message.is_empty() || !message.is_ascii() || message.contains(&0) {
        return Err(SmtpError::invalid(
            "prepared SMTP message must be seven-bit and nonempty",
        ));
    }
    let mut line_start = 0usize;
    let mut header_count = 0usize;
    let mut in_headers = true;
    let mut last_had_header = false;
    let mut index = 0usize;
    while index < message.len() {
        let byte = message[index];
        if byte == b'\n' {
            if index == 0 || message[index - 1] != b'\r' {
                return Err(SmtpError::invalid(
                    "prepared SMTP message requires CRLF lines",
                ));
            }
            let content = &message[line_start..index - 1];
            validate_line(content, in_headers, &mut header_count, &mut last_had_header)?;
            if in_headers && content.is_empty() {
                in_headers = false;
            }
            line_start = index + 1;
        } else if byte == b'\r' {
            if index + 1 >= message.len() || message[index + 1] != b'\n' {
                return Err(SmtpError::invalid(
                    "prepared SMTP message contains a bare CR",
                ));
            }
        } else if (byte < b' ' && byte != b'\t') || byte == 0x7f {
            return Err(SmtpError::invalid(
                "prepared SMTP message contains a control byte",
            ));
        }
        index += 1;
    }
    if line_start < message.len() {
        let content = &message[line_start..];
        validate_line(content, in_headers, &mut header_count, &mut last_had_header)?;
    }
    if in_headers || header_count == 0 {
        return Err(SmtpError::invalid(
            "prepared SMTP message requires headers and a blank separator",
        ));
    }
    Ok(())
}

fn validate_line(
    content: &[u8],
    in_headers: bool,
    header_count: &mut usize,
    last_had_header: &mut bool,
) -> Result<(), SmtpError> {
    if content.len() > 998 {
        return Err(SmtpError::invalid("prepared SMTP line exceeds 998 octets"));
    }
    if !in_headers || content.is_empty() {
        return Ok(());
    }
    if content[0] == b' ' || content[0] == b'\t' {
        if !*last_had_header {
            return Err(SmtpError::invalid(
                "SMTP header continuation has no preceding field",
            ));
        }
        return Ok(());
    }
    let Some(colon) = content.iter().position(|byte| *byte == b':') else {
        return Err(SmtpError::invalid("SMTP header field lacks a colon"));
    };
    if colon == 0
        || !content[..colon]
            .iter()
            .all(|byte| (33..=126).contains(byte) && *byte != b':')
    {
        return Err(SmtpError::invalid("SMTP header field name is invalid"));
    }
    *header_count += 1;
    *last_had_header = true;
    Ok(())
}

/// Stable kind of request admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmtpErrorKind {
    InvalidRequest,
    WrongMode,
    Transport,
}

/// Failure to build or submit an SMTP operation.
#[derive(Clone, Debug)]
pub struct SmtpError {
    kind: SmtpErrorKind,
    detail: &'static str,
    transport_diagnostic: Option<TransportDiagnostic>,
}

impl SmtpError {
    pub(crate) fn new(kind: SmtpErrorKind, detail: &'static str) -> Self {
        Self {
            kind,
            detail,
            transport_diagnostic: None,
        }
    }

    pub(crate) fn transport(error: &nbreq::Error, detail: &'static str) -> Self {
        Self {
            kind: SmtpErrorKind::Transport,
            detail,
            transport_diagnostic: Some(error.into()),
        }
    }

    pub(crate) fn invalid(detail: &'static str) -> Self {
        Self::new(SmtpErrorKind::InvalidRequest, detail)
    }

    #[must_use]
    pub fn kind(&self) -> SmtpErrorKind {
        self.kind
    }

    /// Returns a payload-free NBReq admission error classification when available.
    #[must_use]
    pub fn transport_diagnostic(&self) -> Option<TransportDiagnostic> {
        self.transport_diagnostic
    }
}

impl fmt::Display for SmtpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.detail)
    }
}

impl StdError for SmtpError {}
