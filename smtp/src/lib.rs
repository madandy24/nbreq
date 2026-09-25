//! A bounded SMTP sending client built on NBReq's verified TCP TLS transport.
//!
//! This development crate supports implicit TLS and required STARTTLS. The initial
//! message input is prepared seven-bit RFC 5322 data with canonical CRLF framing.
//! AUTH, SIZE, 8BITMIME, SMTPUTF8, PIPELINING, CHUNKING, retries and MIME construction
//! are outside this first slice.

mod client;
mod request;

pub use client::{
    Delivery, FailureReason, RecipientReport, RecipientStatus, ReplySummary, SendOperation,
    SendOutcome, SmtpClient, Stage,
};
pub use request::{
    Envelope, RecipientPolicy, SendRequest, SmtpError, SmtpErrorKind, SmtpServer, TlsPolicy,
};
