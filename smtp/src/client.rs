use std::task::Poll;

use nbreq::{Engine, RunMode, TcpConnector};

use crate::request::{SendRequest, SmtpError, SmtpErrorKind};

/// SMTP protocol phase in which a submission stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Connect,
    Greeting,
    Ehlo,
    StartTls,
    Upgrade,
    MailFrom,
    RcptTo(usize),
    DataCommand,
    DataBody,
    DataResult,
    Quit,
}

/// Why a submission ended without confirmed acceptance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureReason {
    Cancelled,
    Timeout,
    Transport,
    Protocol,
    EngineStopped,
    StartTlsUnavailable,
    NoAcceptedRecipients,
    RecipientsRejected,
}

/// Bounded summary of a server reply. Text is server supplied and is not authenticated
/// until TLS is established.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplySummary {
    pub code: u16,
    pub text: String,
}

/// The result of one requested RCPT command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecipientStatus {
    /// RCPT was accepted for this transaction; it is not proof of message delivery.
    AcceptedForTransaction { reply: ReplySummary },
    Rejected { reply: ReplySummary },
    /// No RCPT command was admitted for this recipient.
    Unattempted,
    /// RCPT was admitted, but no complete valid response was observed.
    Unresolved,
}

/// Outcome for one requested recipient in original request order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipientReport {
    pub address: String,
    pub status: RecipientStatus,
}

/// Outcome of SMTP message acceptance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Delivery {
    /// The server returned final 250 after the complete DATA terminator.
    Accepted { reply: ReplySummary },
    /// The server explicitly rejected the transaction.
    Rejected { stage: Stage, reply: ReplySummary },
    /// No final acceptance occurred and the DATA terminator was not fully admitted.
    NotAccepted { stage: Stage, reason: FailureReason },
    /// The terminator was fully admitted but no valid final acceptance/rejection was read.
    Uncertain { stage: Stage, reason: FailureReason },
}

/// Stable terminal outcome, including all requested recipients.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendOutcome {
    pub recipients: Vec<RecipientReport>,
    pub delivery: Delivery,
}

/// Engine-bound SMTP sending capability. It does not own the Engine.
#[derive(Clone, Debug)]
pub struct SmtpClient {
    connector: TcpConnector,
    mode: RunMode,
}

impl SmtpClient {
    /// Creates a sender for an existing NBReq Engine.
    #[must_use]
    pub fn new(engine: &Engine) -> Self {
        Self {
            connector: engine.tcp_connector(),
            mode: engine.run_mode(),
        }
    }

    /// Submits an owned request for caller-driven nonblocking protocol progress.
    pub fn submit(&self, request: SendRequest) -> Result<SendOperation, SmtpError> {
        let _ = &self.connector;
        let recipients = request
            .envelope
            .recipients
            .iter()
            .map(|address| RecipientReport {
                address: address.clone(),
                status: RecipientStatus::Unattempted,
            })
            .collect();
        Ok(SendOperation {
            outcome: SendOutcome {
                recipients,
                delivery: Delivery::NotAccepted {
                    stage: Stage::Connect,
                    reason: FailureReason::Protocol,
                },
            },
        })
    }

    /// Runs a submission to completion on a spawned Engine.
    pub fn send_blocking(&self, request: SendRequest) -> Result<SendOutcome, SmtpError> {
        if self.mode != RunMode::Spawned {
            return Err(SmtpError::new(
                SmtpErrorKind::WrongMode,
                "blocking SMTP send requires a spawned Engine",
            ));
        }
        let mut operation = self.submit(request)?;
        match operation.poll() {
            Poll::Ready(outcome) => Ok(outcome.clone()),
            Poll::Pending => Err(SmtpError::new(
                SmtpErrorKind::InvalidRequest,
                "SMTP operation did not finish",
            )),
        }
    }
}

/// One owned, cancellable SMTP submission.
pub struct SendOperation {
    outcome: SendOutcome,
}

impl SendOperation {
    /// Makes bounded nonblocking progress. A manual Engine must be driven separately.
    /// After completion, repeated polls return the same outcome.
    pub fn poll(&mut self) -> Poll<&SendOutcome> {
        Poll::Ready(&self.outcome)
    }

    /// Cancels an in-progress submission. Every terminal outcome remains stable.
    pub fn cancel(&mut self) {
        // The compiling scaffold already has a terminal outcome. Runtime cancellation
        // behavior is added after independent behavioral-red tests are recorded.
    }
}
