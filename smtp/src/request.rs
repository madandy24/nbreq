use std::error::Error as StdError;
use std::fmt;
use std::time::Duration;

use nbreq::{TcpConnectRequest, TlsOptions};

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
        Ok(Self {
            from: from.into(),
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
        Ok(Self {
            server,
            ehlo_name: ehlo_name.into(),
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
}

/// Stable kind of request admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmtpErrorKind {
    InvalidRequest,
    WrongMode,
}

/// Failure to build or submit an SMTP operation.
#[derive(Clone, Debug)]
pub struct SmtpError {
    kind: SmtpErrorKind,
    detail: &'static str,
}

impl SmtpError {
    pub(crate) fn new(kind: SmtpErrorKind, detail: &'static str) -> Self {
        Self { kind, detail }
    }

    #[must_use]
    pub fn kind(&self) -> SmtpErrorKind {
        self.kind
    }
}

impl fmt::Display for SmtpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.detail)
    }
}

impl StdError for SmtpError {}
