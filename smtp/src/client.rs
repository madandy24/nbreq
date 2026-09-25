use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use nbreq::{
    DnsFailure, Engine, Error, ErrorKind, LimitKind, PendingTcpConnect, PendingTlsConnect, RunMode,
    TcpConnectCompletion, TcpConnectWaitOutcome, TcpConnection, TcpConnector, TcpFinishError,
    TcpFinishStatus, TcpRead, TcpSendErrorKind, TcpStreamError, TimeoutKind, TlsConnectCompletion,
    TlsConnectWaitOutcome, TlsConnection, TlsFailure, TransportStage,
};

use crate::data::DataEncoder;
use crate::reply::{Reply, ReplyParser};
use crate::request::{RecipientPolicy, SendRequest, SmtpError, SmtpErrorKind, TlsPolicy};

const MAX_STEPS_PER_POLL: usize = 64;
const MAX_DATA_BYTES_PER_POLL: usize = 16 * 1024;
// Outbound retains one source chunk and one owned candidate for NBReq admission.
// Two 2 KiB chunks keep their combined DATA staging within 4 KiB.
const MAX_STAGING: usize = 2048;

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
    AcceptedForTransaction {
        reply: ReplySummary,
    },
    Rejected {
        reply: ReplySummary,
    },
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
    /// The server returned a final positive 2xx reply after the complete DATA terminator.
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
    /// A payload-free snapshot of the NBReq transport error, when available.
    pub transport_diagnostic: Option<TransportDiagnostic>,
}

/// Structured transport classification without peer text, message data, or credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportDiagnostic {
    pub kind: ErrorKind,
    pub transport_stage: Option<TransportStage>,
    pub tls_failure: Option<TlsFailure>,
    pub dns_failure: Option<DnsFailure>,
    pub timeout_kind: Option<TimeoutKind>,
    pub limit_kind: Option<LimitKind>,
}

impl From<&Error> for TransportDiagnostic {
    fn from(error: &Error) -> Self {
        Self {
            kind: error.kind(),
            transport_stage: error.transport_stage(),
            tls_failure: error.tls_failure(),
            dns_failure: error.dns_failure(),
            timeout_kind: error.timeout_kind(),
            limit_kind: error.limit_kind(),
        }
    }
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
        request.validate_deadlines()?;
        let now = Instant::now();
        let overall_deadline = now
            .checked_add(request.overall_timeout)
            .ok_or_else(|| SmtpError::invalid("SMTP overall deadline cannot be represented"))?;
        let phase_deadline = now
            .checked_add(request.command_timeout)
            .ok_or_else(|| SmtpError::invalid("SMTP command deadline cannot be represented"))?;
        let send_limit = request
            .server
            .connect
            .send_queue_bytes()
            .unwrap_or(MAX_STAGING)
            .min(MAX_STAGING);
        let pending = match request.server.policy {
            TlsPolicy::Implicit => PendingConnect::Tls(
                self.connector
                    .submit_tls(request.server.connect.clone(), request.server.tls.clone())
                    .map_err(|error| {
                        SmtpError::transport(&error, "SMTP TLS connection admission failed")
                    })?,
            ),
            TlsPolicy::RequiredStartTls => PendingConnect::Plain(
                self.connector
                    .submit(request.server.connect.clone())
                    .map_err(|error| {
                        SmtpError::transport(&error, "SMTP TCP connection admission failed")
                    })?,
            ),
        };
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
            request: Some(request),
            phase: Phase::Connect,
            pending: Some(pending),
            transport: None,
            parser: ReplyParser::default(),
            outbound: None,
            data: None,
            reports: recipients,
            outcome: None,
            accepted: None,
            transport_diagnostic: None,
            terminator_admitted: false,
            send_limit,
            phase_deadline,
            overall_deadline,
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
        loop {
            match operation.poll() {
                Poll::Ready(_) => {
                    return operation.outcome.take().ok_or_else(|| {
                        SmtpError::new(SmtpErrorKind::Transport, "SMTP terminal outcome was lost")
                    });
                }
                Poll::Pending => thread::sleep(Duration::from_millis(1)),
            }
        }
    }
}

/// One owned, cancellable SMTP submission.
pub struct SendOperation {
    request: Option<SendRequest>,
    phase: Phase,
    pending: Option<PendingConnect>,
    transport: Option<Transport>,
    parser: ReplyParser,
    outbound: Option<Outbound>,
    data: Option<DataEncoder>,
    reports: Vec<RecipientReport>,
    outcome: Option<SendOutcome>,
    accepted: Option<ReplySummary>,
    transport_diagnostic: Option<TransportDiagnostic>,
    terminator_admitted: bool,
    send_limit: usize,
    phase_deadline: Instant,
    overall_deadline: Instant,
}

impl SendOperation {
    /// Makes bounded nonblocking progress. A manual Engine must be driven separately.
    /// After completion, repeated polls return the same outcome.
    pub fn poll(&mut self) -> Poll<&SendOutcome> {
        let mut data_bytes = 0usize;
        for _ in 0..MAX_STEPS_PER_POLL {
            if self.outcome.is_some() {
                break;
            }
            let now = Instant::now();
            if now >= self.overall_deadline || now >= self.phase_deadline {
                self.fail(FailureReason::Timeout);
                break;
            }
            if !self.advance(&mut data_bytes) {
                break;
            }
        }
        match self.outcome.as_ref() {
            Some(outcome) => Poll::Ready(outcome),
            None => Poll::Pending,
        }
    }

    /// Cancels an in-progress submission. Every terminal outcome remains stable.
    pub fn cancel(&mut self) {
        if self.outcome.is_none() {
            self.fail(FailureReason::Cancelled);
        }
    }
}

impl Drop for SendOperation {
    fn drop(&mut self) {
        self.abort_transport();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Connect,
    Greeting,
    EhloSend,
    EhloReply,
    StartTlsSend,
    StartTlsReply,
    Upgrade,
    MailSend,
    MailReply,
    RcptSend(usize),
    RcptReply(usize),
    DataSend,
    DataReply,
    DataBody,
    TerminatorSend,
    DataResult,
    QuitSend,
    QuitReply,
    Finish,
}

impl Phase {
    fn public(self) -> Stage {
        match self {
            Self::Connect => Stage::Connect,
            Self::Greeting => Stage::Greeting,
            Self::EhloSend | Self::EhloReply => Stage::Ehlo,
            Self::StartTlsSend | Self::StartTlsReply => Stage::StartTls,
            Self::Upgrade => Stage::Upgrade,
            Self::MailSend | Self::MailReply => Stage::MailFrom,
            Self::RcptSend(index) | Self::RcptReply(index) => Stage::RcptTo(index),
            Self::DataSend | Self::DataReply => Stage::DataCommand,
            Self::DataBody | Self::TerminatorSend => Stage::DataBody,
            Self::DataResult => Stage::DataResult,
            Self::QuitSend | Self::QuitReply | Self::Finish => Stage::Quit,
        }
    }
}

enum PendingConnect {
    Plain(PendingTcpConnect),
    Tls(PendingTlsConnect),
}

enum Transport {
    Plain(TcpConnection),
    Tls(TlsConnection),
}

impl Transport {
    fn try_read(&mut self, destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        match self {
            Self::Plain(connection) => connection.try_read(destination),
            Self::Tls(connection) => connection.try_read(destination),
        }
    }

    fn try_send(&mut self, bytes: Vec<u8>) -> Result<(), nbreq::TcpSendError> {
        match self {
            Self::Plain(connection) => connection.try_send(bytes),
            Self::Tls(connection) => connection.try_send(bytes),
        }
    }

    fn cancel(&self) {
        match self {
            Self::Plain(connection) => {
                let _ = connection.handle().cancel();
            }
            Self::Tls(connection) => {
                let _ = connection.handle().cancel();
            }
        }
    }

    fn try_finish(&mut self) -> Result<TcpFinishStatus, TcpFinishError> {
        match self {
            Self::Plain(connection) => connection.try_finish(),
            Self::Tls(connection) => connection.try_finish(),
        }
    }
}

struct Outbound {
    bytes: Vec<u8>,
    offset: usize,
    retry: Option<Vec<u8>>,
}

enum SendStep {
    Pending,
    Progress(usize),
    Done(usize),
    Failed(FailureReason),
}

impl Outbound {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            offset: 0,
            retry: None,
        }
    }

    fn step(&mut self, transport: &mut Transport, limit: &mut usize) -> SendStep {
        if self.offset == self.bytes.len() {
            return SendStep::Done(0);
        }
        let candidate = self.retry.take().unwrap_or_else(|| {
            let end = (self.offset + *limit).min(self.bytes.len());
            self.bytes[self.offset..end].to_vec()
        });
        let length = candidate.len();
        match transport.try_send(candidate) {
            Ok(()) => {
                self.offset += length;
                if self.offset == self.bytes.len() {
                    SendStep::Done(length)
                } else {
                    SendStep::Progress(length)
                }
            }
            Err(error) if error.kind() == TcpSendErrorKind::WouldBlock => {
                self.retry = Some(error.into_remaining());
                SendStep::Pending
            }
            Err(error) if error.kind() == TcpSendErrorKind::ChunkTooLarge && *limit > 1 => {
                *limit = (*limit / 2).max(1);
                SendStep::Progress(0)
            }
            Err(error) => SendStep::Failed(match error.kind() {
                TcpSendErrorKind::EngineStopped => FailureReason::EngineStopped,
                TcpSendErrorKind::Cancelled => FailureReason::Cancelled,
                _ => FailureReason::Transport,
            }),
        }
    }
}

enum ReadStep {
    Pending,
    Progress,
    Complete(Reply),
    Failed(FailureReason),
}

impl SendOperation {
    fn request(&self) -> &SendRequest {
        self.request
            .as_ref()
            .expect("active SMTP operation has a request")
    }

    fn reason_from_error(error: &Error) -> FailureReason {
        match error.kind() {
            ErrorKind::Timeout => FailureReason::Timeout,
            ErrorKind::EngineStopped => FailureReason::EngineStopped,
            _ => FailureReason::Transport,
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        let now = Instant::now();
        self.phase_deadline = now
            .checked_add(self.request().command_timeout)
            .unwrap_or(self.overall_deadline)
            .min(self.overall_deadline);
    }

    fn command(&mut self, phase: Phase, bytes: Vec<u8>) {
        self.set_phase(phase);
        self.outbound = Some(Outbound::new(bytes));
    }

    fn abort_transport(&mut self) {
        if let Some(pending) = self.pending.take() {
            match pending {
                PendingConnect::Plain(connection) => {
                    let _ = connection.handle().cancel();
                }
                PendingConnect::Tls(connection) => {
                    let _ = connection.handle().cancel();
                }
            }
        }
        if let Some(connection) = self.transport.take() {
            connection.cancel();
        }
        self.outbound = None;
        self.data = None;
    }

    fn terminal(&mut self, delivery: Delivery) {
        if self.outcome.is_some() {
            return;
        }
        self.abort_transport();
        self.request = None;
        self.outcome = Some(SendOutcome {
            recipients: std::mem::take(&mut self.reports),
            delivery,
            transport_diagnostic: self.transport_diagnostic.take(),
        });
    }

    fn fail(&mut self, reason: FailureReason) {
        if let Some(reply) = self.accepted.take() {
            self.terminal(Delivery::Accepted { reply });
        } else if self.terminator_admitted {
            self.terminal(Delivery::Uncertain {
                stage: self.phase.public(),
                reason,
            });
        } else {
            self.terminal(Delivery::NotAccepted {
                stage: self.phase.public(),
                reason,
            });
        }
    }

    fn reject(&mut self, reply: Reply) {
        self.terminal(Delivery::Rejected {
            stage: self.phase.public(),
            reply: reply.summary(),
        });
    }

    fn poll_connect(&mut self) -> bool {
        let Some(pending) = self.pending.take() else {
            self.fail(FailureReason::Protocol);
            return false;
        };
        match pending {
            PendingConnect::Plain(pending) => match pending.wait_for(Duration::ZERO) {
                TcpConnectWaitOutcome::TimedOut(pending) => {
                    self.pending = Some(PendingConnect::Plain(pending));
                    false
                }
                TcpConnectWaitOutcome::Completed(completion) => {
                    match completion {
                        TcpConnectCompletion::Completed(connection) => {
                            self.transport = Some(Transport::Plain(connection));
                            self.set_phase(Phase::Greeting);
                        }
                        TcpConnectCompletion::Failed(error) => {
                            self.transport_diagnostic = Some((&error).into());
                            self.fail(Self::reason_from_error(&error));
                        }
                        TcpConnectCompletion::Cancelled => self.fail(FailureReason::Cancelled),
                        _ => self.fail(FailureReason::Protocol),
                    }
                    true
                }
                _ => {
                    self.fail(FailureReason::Protocol);
                    false
                }
            },
            PendingConnect::Tls(pending) => match pending.wait_for(Duration::ZERO) {
                TlsConnectWaitOutcome::TimedOut(pending) => {
                    self.pending = Some(PendingConnect::Tls(pending));
                    false
                }
                TlsConnectWaitOutcome::Completed(completion) => {
                    match completion {
                        TlsConnectCompletion::Completed(connection) => {
                            self.transport = Some(Transport::Tls(connection));
                            if self.phase == Phase::Upgrade {
                                let command =
                                    format!("EHLO {}\r\n", self.request().ehlo_name).into_bytes();
                                self.command(Phase::EhloSend, command);
                            } else {
                                self.set_phase(Phase::Greeting);
                            }
                        }
                        TlsConnectCompletion::Failed(error) => {
                            self.transport_diagnostic = Some((&error).into());
                            self.fail(Self::reason_from_error(&error));
                        }
                        TlsConnectCompletion::Cancelled => self.fail(FailureReason::Cancelled),
                        _ => self.fail(FailureReason::Protocol),
                    }
                    true
                }
                _ => {
                    self.fail(FailureReason::Protocol);
                    false
                }
            },
        }
    }

    fn read_step(&mut self) -> ReadStep {
        let Some(transport) = self.transport.as_mut() else {
            return ReadStep::Failed(FailureReason::Transport);
        };
        let mut byte = [0u8; 1];
        match transport.try_read(&mut byte) {
            Ok(TcpRead::Pending) => ReadStep::Pending,
            Ok(TcpRead::Data(1)) => match self.parser.push(byte[0]) {
                Ok(Some(reply)) => ReadStep::Complete(reply),
                Ok(None) => ReadStep::Progress,
                Err(()) => ReadStep::Failed(FailureReason::Protocol),
            },
            Ok(TcpRead::Eof) | Ok(TcpRead::Data(_)) => ReadStep::Failed(FailureReason::Transport),
            Err(TcpStreamError::Failed(error) | TcpStreamError::Operation(error)) => {
                self.transport_diagnostic = Some((&error).into());
                ReadStep::Failed(Self::reason_from_error(&error))
            }
            Err(TcpStreamError::Cancelled) => ReadStep::Failed(FailureReason::Cancelled),
            Err(TcpStreamError::Reset) => ReadStep::Failed(FailureReason::Transport),
            Ok(_) => ReadStep::Failed(FailureReason::Protocol),
            Err(_) => ReadStep::Failed(FailureReason::Transport),
        }
    }

    fn send_step(&mut self) -> SendStep {
        match (self.outbound.as_mut(), self.transport.as_mut()) {
            (Some(outbound), Some(transport)) => outbound.step(transport, &mut self.send_limit),
            _ => SendStep::Failed(FailureReason::Transport),
        }
    }

    fn advance(&mut self, data_bytes: &mut usize) -> bool {
        match self.phase {
            Phase::Connect | Phase::Upgrade => self.poll_connect(),
            Phase::Greeting
            | Phase::EhloReply
            | Phase::StartTlsReply
            | Phase::MailReply
            | Phase::RcptReply(_)
            | Phase::DataReply
            | Phase::DataResult
            | Phase::QuitReply => match self.read_step() {
                ReadStep::Pending => false,
                ReadStep::Progress => true,
                ReadStep::Complete(reply) => {
                    self.handle_reply(reply);
                    true
                }
                ReadStep::Failed(reason) => {
                    self.fail(reason);
                    false
                }
            },
            Phase::EhloSend
            | Phase::StartTlsSend
            | Phase::MailSend
            | Phase::RcptSend(_)
            | Phase::DataSend
            | Phase::TerminatorSend
            | Phase::QuitSend => match self.send_step() {
                SendStep::Pending => false,
                SendStep::Progress(_) => true,
                SendStep::Done(_) => {
                    self.outbound = None;
                    self.after_command();
                    true
                }
                SendStep::Failed(reason) => {
                    self.fail(reason);
                    false
                }
            },
            Phase::DataBody => {
                if *data_bytes >= MAX_DATA_BYTES_PER_POLL {
                    return false;
                }
                if self.outbound.is_none() {
                    let request = self.request.as_ref().expect("active request");
                    let data = self.data.as_mut().expect("DATA encoder initialized");
                    if data.finished(&request.message) {
                        self.command(Phase::TerminatorSend, b".\r\n".to_vec());
                        return true;
                    }
                    let limit = MAX_STAGING.min(MAX_DATA_BYTES_PER_POLL - *data_bytes);
                    self.outbound = Some(Outbound::new(data.next_chunk(&request.message, limit)));
                }
                match self.send_step() {
                    SendStep::Pending => false,
                    SendStep::Progress(bytes) => {
                        *data_bytes += bytes;
                        true
                    }
                    SendStep::Done(bytes) => {
                        *data_bytes += bytes;
                        self.outbound = None;
                        true
                    }
                    SendStep::Failed(reason) => {
                        self.fail(reason);
                        false
                    }
                }
            }
            Phase::Finish => {
                let Some(transport) = self.transport.as_mut() else {
                    self.fail(FailureReason::Transport);
                    return false;
                };
                match transport.try_finish() {
                    Ok(TcpFinishStatus::Pending) => false,
                    Ok(TcpFinishStatus::Finished) => {
                        self.fail(FailureReason::Protocol);
                        true
                    }
                    Err(_) | Ok(_) => {
                        self.fail(FailureReason::Transport);
                        false
                    }
                }
            }
        }
    }

    fn after_command(&mut self) {
        match self.phase {
            Phase::EhloSend => self.phase = Phase::EhloReply,
            Phase::StartTlsSend => self.phase = Phase::StartTlsReply,
            Phase::MailSend => self.phase = Phase::MailReply,
            Phase::RcptSend(index) => {
                self.reports[index].status = RecipientStatus::Unresolved;
                self.phase = Phase::RcptReply(index);
            }
            Phase::DataSend => self.phase = Phase::DataReply,
            Phase::TerminatorSend => {
                self.terminator_admitted = true;
                self.set_phase(Phase::DataResult);
            }
            Phase::QuitSend => self.phase = Phase::QuitReply,
            _ => self.fail(FailureReason::Protocol),
        }
    }

    fn start_mail(&mut self) {
        let from = &self.request().envelope.from;
        let command = if from.is_empty() {
            b"MAIL FROM:<>\r\n".to_vec()
        } else {
            format!("MAIL FROM:<{from}>\r\n").into_bytes()
        };
        self.command(Phase::MailSend, command);
    }

    fn start_rcpt(&mut self, index: usize) {
        let address = &self.request().envelope.recipients[index];
        self.command(
            Phase::RcptSend(index),
            format!("RCPT TO:<{address}>\r\n").into_bytes(),
        );
    }

    fn handle_reply(&mut self, reply: Reply) {
        let code = reply.code;
        let negative = (400..600).contains(&code);
        match self.phase {
            Phase::Greeting => {
                if (200..300).contains(&code) {
                    let command = format!("EHLO {}\r\n", self.request().ehlo_name).into_bytes();
                    self.command(Phase::EhloSend, command);
                } else if negative {
                    self.reject(reply);
                } else {
                    self.fail(FailureReason::Protocol);
                }
            }
            Phase::EhloReply => {
                if (200..300).contains(&code) {
                    if self.request().server.policy == TlsPolicy::RequiredStartTls
                        && matches!(self.transport, Some(Transport::Plain(_)))
                    {
                        if reply.advertises("STARTTLS") {
                            self.command(Phase::StartTlsSend, b"STARTTLS\r\n".to_vec());
                        } else {
                            self.fail(FailureReason::StartTlsUnavailable);
                        }
                    } else {
                        self.start_mail();
                    }
                } else if negative {
                    self.reject(reply);
                } else {
                    self.fail(FailureReason::Protocol);
                }
            }
            Phase::StartTlsReply => {
                if code == 220 {
                    let Some(Transport::Plain(connection)) = self.transport.take() else {
                        self.fail(FailureReason::Protocol);
                        return;
                    };
                    match connection.submit_tls(self.request().server.tls.clone()) {
                        Ok(pending) => {
                            self.pending = Some(PendingConnect::Tls(pending));
                            self.set_phase(Phase::Upgrade);
                        }
                        Err(error) => {
                            self.transport_diagnostic = Some((&error).into());
                            self.fail(Self::reason_from_error(&error));
                        }
                    }
                } else if negative {
                    self.reject(reply);
                } else {
                    self.fail(FailureReason::Protocol);
                }
            }
            Phase::MailReply => {
                if (200..300).contains(&code) {
                    self.start_rcpt(0);
                } else if negative {
                    self.reject(reply);
                } else {
                    self.fail(FailureReason::Protocol);
                }
            }
            Phase::RcptReply(index) => {
                if (200..300).contains(&code) {
                    self.reports[index].status = RecipientStatus::AcceptedForTransaction {
                        reply: reply.summary(),
                    };
                } else if negative {
                    self.reports[index].status = RecipientStatus::Rejected {
                        reply: reply.summary(),
                    };
                    if code == 421 {
                        self.reject(reply);
                        return;
                    }
                } else {
                    self.fail(FailureReason::Protocol);
                    return;
                }
                if index + 1 < self.request().envelope.recipients.len() {
                    self.start_rcpt(index + 1);
                } else {
                    let accepted = self
                        .reports
                        .iter()
                        .filter(|report| {
                            matches!(
                                report.status,
                                RecipientStatus::AcceptedForTransaction { .. }
                            )
                        })
                        .count();
                    let rejected = self
                        .reports
                        .iter()
                        .any(|report| matches!(report.status, RecipientStatus::Rejected { .. }));
                    if accepted == 0 {
                        self.fail(FailureReason::NoAcceptedRecipients);
                    } else if rejected
                        && self.request().recipient_policy == RecipientPolicy::RequireAllRecipients
                    {
                        self.fail(FailureReason::RecipientsRejected);
                    } else {
                        self.command(Phase::DataSend, b"DATA\r\n".to_vec());
                    }
                }
            }
            Phase::DataReply => {
                if (300..400).contains(&code) {
                    self.data = Some(DataEncoder::new(&self.request().message));
                    self.set_phase(Phase::DataBody);
                } else if negative {
                    self.reject(reply);
                } else {
                    self.fail(FailureReason::Protocol);
                }
            }
            Phase::DataResult => {
                if (200..300).contains(&code) {
                    self.accepted = Some(reply.summary());
                    self.command(Phase::QuitSend, b"QUIT\r\n".to_vec());
                } else if negative {
                    self.reject(reply);
                } else {
                    self.fail(FailureReason::Protocol);
                }
            }
            Phase::QuitReply => {
                if (200..300).contains(&code) && self.accepted.is_some() {
                    self.set_phase(Phase::Finish);
                } else {
                    self.fail(FailureReason::Protocol);
                }
            }
            _ => self.fail(FailureReason::Protocol),
        }
    }
}
