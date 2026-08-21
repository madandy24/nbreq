use std::cell::Cell;
use std::collections::VecDeque;
use std::error::Error as StdError;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::{
    Error, ErrorKind, Header, LimitKind, Method, Request, RequestBuilder, RequestHandle,
    RequestOptions, Response, RunMode, TlsVerification,
};

type StreamWaker = Arc<dyn Fn() + Send + Sync + 'static>;

/// The Engine-owned receiving half of one streamed request body.
///
/// Create it together with its unique [`UploadSender`], retain the sender, and move this value into
/// [`StreamRequestBuilder::body_stream`]. Neither half is cloneable.
///
/// ```compile_fail
/// fn require_sync<T: Sync>() {}
/// require_sync::<nbreq::UploadBody>();
/// ```
pub struct UploadBody {
    shared: Arc<UploadShared>,
    not_sync: PhantomData<Cell<()>>,
}

impl UploadBody {
    /// Creates a fixed-length upload pair with a bounded pre-transport queue.
    ///
    /// NBReq will generate `Content-Length` when this body is submitted. The sender must accept
    /// exactly `length` bytes and be explicitly finished.
    pub fn fixed(length: u64, queue_capacity: usize) -> Result<(Self, UploadSender), Error> {
        Self::pair(UploadFraming::Fixed(length), queue_capacity)
    }

    /// Creates an unknown-length HTTP/1.1 chunked upload pair with a bounded pre-transport queue.
    ///
    /// NBReq will generate `Transfer-Encoding: chunked` when this body is submitted. Finishing the
    /// sender terminates the body; request trailers are not part of this initial contract.
    pub fn chunked(queue_capacity: usize) -> Result<(Self, UploadSender), Error> {
        Self::pair(UploadFraming::Chunked, queue_capacity)
    }

    fn pair(framing: UploadFraming, queue_capacity: usize) -> Result<(Self, UploadSender), Error> {
        if queue_capacity == 0 {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "a streamed upload queue capacity must be greater than zero",
            ));
        }
        let shared = Arc::new(UploadShared {
            state: Mutex::new(UploadState {
                queue: VecDeque::new(),
                queued_bytes: 0,
                accepted_bytes: 0,
                producer: ProducerState::Open,
                receiver_alive: true,
                total_limit: None,
                run_mode: None,
            }),
            changed: Condvar::new(),
            engine_waker: Mutex::new(None),
            framing,
            queue_capacity,
        });
        Ok((
            Self {
                shared: Arc::clone(&shared),
                not_sync: PhantomData,
            },
            UploadSender {
                shared,
                terminated: false,
                not_sync: PhantomData,
            },
        ))
    }

    /// Returns the maximum number of caller bytes that this transfer can queue before backpressure.
    #[must_use]
    pub fn queue_capacity(&self) -> usize {
        self.shared.queue_capacity
    }

    /// Returns the declared body length, or `None` for a chunked body.
    #[must_use]
    pub fn declared_length(&self) -> Option<u64> {
        match self.shared.framing {
            UploadFraming::Fixed(length) => Some(length),
            UploadFraming::Chunked => None,
        }
    }

    fn validate_for_build(&self) -> Result<(), Error> {
        let state = lock_unpoisoned(&self.shared.state);
        match state.producer {
            ProducerState::Open | ProducerState::Finished => Ok(()),
            ProducerState::Abandoned => Err(Error::new(
                ErrorKind::InvalidRequest,
                "the streamed upload sender was dropped before finish",
            )),
            ProducerState::LengthMismatch => Err(Error::new(
                ErrorKind::InvalidRequest,
                "the fixed-length streamed upload was finished at the wrong length",
            )),
        }
    }

    #[allow(dead_code)] // The native submission seam binds this in the next slice.
    pub(crate) fn bind(
        &self,
        max_queue_capacity: usize,
        total_limit: usize,
        run_mode: RunMode,
        engine_waker: StreamWaker,
    ) -> Result<(), Error> {
        if self.shared.queue_capacity > max_queue_capacity {
            return Err(Error::limit(
                LimitKind::StreamingQueueBytes,
                format!(
                    "streamed upload queue exceeds the configured {max_queue_capacity} byte per-transfer limit"
                ),
            ));
        }
        let total_limit = u64::try_from(total_limit).unwrap_or(u64::MAX);
        let mut state = lock_unpoisoned(&self.shared.state);
        if state.total_limit.is_some() {
            return Err(Error::new(
                ErrorKind::Internal,
                "a streamed upload body was bound to an Engine more than once",
            ));
        }
        if !state.receiver_alive {
            return Err(Error::transport(
                crate::TransportStage::Send,
                "the streamed upload body closed before submission",
            ));
        }
        match state.producer {
            ProducerState::Open | ProducerState::Finished => {}
            ProducerState::Abandoned => {
                return Err(Error::transport(
                    crate::TransportStage::Send,
                    "the streamed upload sender was dropped before submission",
                ));
            }
            ProducerState::LengthMismatch => {
                return Err(Error::transport(
                    crate::TransportStage::Send,
                    "the fixed-length streamed upload finished at the wrong length",
                ));
            }
        }
        if state.accepted_bytes > total_limit
            || matches!(self.shared.framing, UploadFraming::Fixed(length) if length > total_limit)
        {
            return Err(Error::limit(
                LimitKind::RequestBodyBytes,
                format!("streamed request body exceeds the configured {total_limit} byte limit"),
            ));
        }
        state.total_limit = Some(total_limit);
        state.run_mode = Some(run_mode);
        *lock_unpoisoned(&self.shared.engine_waker) = Some(engine_waker);
        drop(state);
        Ok(())
    }

    #[allow(dead_code)] // The native HTTP owner consumes this in the next slice.
    pub(crate) fn try_pop(&mut self) -> UploadPoll {
        let mut state = lock_unpoisoned(&self.shared.state);
        if !state.receiver_alive {
            return UploadPoll::Failed(Error::transport(
                crate::TransportStage::Send,
                "the streamed upload body is closed",
            ));
        }
        if let Some(chunk) = state.queue.pop_front() {
            state.queued_bytes -= chunk.len();
            drop(state);
            self.shared.changed.notify_all();
            return UploadPoll::Chunk(chunk);
        }
        match state.producer {
            ProducerState::Open => UploadPoll::Pending,
            ProducerState::Finished => UploadPoll::Finished,
            ProducerState::Abandoned => UploadPoll::Failed(Error::transport(
                crate::TransportStage::Send,
                "the streamed upload sender was dropped before finish",
            )),
            ProducerState::LengthMismatch => UploadPoll::Failed(Error::transport(
                crate::TransportStage::Send,
                "the fixed-length streamed upload finished at the wrong length",
            )),
        }
    }

    #[cfg(feature = "native")]
    pub(crate) fn framing(&self) -> UploadFraming {
        self.shared.framing
    }

    #[allow(dead_code)] // Early response, cancellation, and shutdown call this after submission.
    pub(crate) fn close(&mut self) {
        let mut state = lock_unpoisoned(&self.shared.state);
        state.receiver_alive = false;
        state.queue.clear();
        state.queued_bytes = 0;
        drop(state);
        self.shared.changed.notify_all();
    }
}

impl fmt::Debug for UploadBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UploadBody")
            .field("declared_length", &self.declared_length())
            .field("queue_capacity", &self.queue_capacity())
            .finish_non_exhaustive()
    }
}

impl Drop for UploadBody {
    fn drop(&mut self) {
        let mut state = lock_unpoisoned(&self.shared.state);
        state.receiver_alive = false;
        state.queue.clear();
        state.queued_bytes = 0;
        drop(state);
        self.shared.changed.notify_all();
    }
}

/// The caller-owned producer for one streamed request body.
///
/// This handle is unique and movable between threads. Dropping it before [`Self::finish`] abandons
/// the upload. A blocking producer adapter will be added with Engine submission; this first seam is
/// deliberately nonblocking.
///
/// ```compile_fail
/// fn require_sync<T: Sync>() {}
/// require_sync::<nbreq::UploadSender>();
/// ```
pub struct UploadSender {
    shared: Arc<UploadShared>,
    terminated: bool,
    not_sync: PhantomData<Cell<()>>,
}

impl UploadSender {
    /// Attempts to queue one complete owned chunk without blocking.
    ///
    /// Queue pressure and validation failures return the original `Vec` unchanged. An empty chunk
    /// is a harmless no-op; only [`Self::finish`] terminates either fixed or chunked uploads.
    pub fn try_push(&mut self, chunk: Vec<u8>) -> Result<(), TryPushError> {
        if chunk.len() > self.shared.queue_capacity {
            return Err(TryPushError::new(TryPushErrorKind::ChunkTooLarge, chunk));
        }

        let mut state = lock_unpoisoned(&self.shared.state);
        if !state.receiver_alive {
            return Err(TryPushError::new(TryPushErrorKind::Closed, chunk));
        }
        if state.producer != ProducerState::Open {
            return Err(TryPushError::new(TryPushErrorKind::Closed, chunk));
        }

        let chunk_len = match u64::try_from(chunk.len()) {
            Ok(length) => length,
            Err(_) => {
                return Err(TryPushError::new(TryPushErrorKind::LengthExceeded, chunk));
            }
        };
        let Some(next_accepted) = state.accepted_bytes.checked_add(chunk_len) else {
            return Err(TryPushError::new(TryPushErrorKind::LengthExceeded, chunk));
        };
        if state
            .total_limit
            .is_some_and(|total_limit| next_accepted > total_limit)
        {
            return Err(TryPushError::new(
                TryPushErrorKind::TotalLimitExceeded,
                chunk,
            ));
        }
        if matches!(self.shared.framing, UploadFraming::Fixed(length) if next_accepted > length) {
            return Err(TryPushError::new(TryPushErrorKind::LengthExceeded, chunk));
        }
        if chunk.len() > self.shared.queue_capacity - state.queued_bytes {
            return Err(TryPushError::new(TryPushErrorKind::WouldBlock, chunk));
        }

        state.accepted_bytes = next_accepted;
        state.queued_bytes += chunk.len();
        if !chunk.is_empty() {
            state.queue.push_back(chunk);
        }
        drop(state);
        self.shared.changed.notify_all();
        if let Some(waker) = lock_unpoisoned(&self.shared.engine_waker).clone() {
            waker();
        }
        Ok(())
    }

    /// Queues all caller bytes, waiting for transport capacity on a spawned Engine.
    ///
    /// The input may be larger than the transfer's queue window and is admitted progressively.
    /// If an early response, cancellation, failure, or Engine stop closes the receiver, the error
    /// returns only the suffix that was not accepted. Call this only after successful submission;
    /// manual Engines reject it without driving the owner.
    pub fn push(&mut self, chunk: Vec<u8>) -> Result<(), TryPushError> {
        let length = match u64::try_from(chunk.len()) {
            Ok(length) => length,
            Err(_) => {
                return Err(TryPushError::new(TryPushErrorKind::LengthExceeded, chunk));
            }
        };
        {
            let state = lock_unpoisoned(&self.shared.state);
            match state.run_mode {
                None => {
                    return Err(TryPushError::new(TryPushErrorKind::NotSubmitted, chunk));
                }
                Some(RunMode::Manual) => {
                    return Err(TryPushError::new(TryPushErrorKind::WrongMode, chunk));
                }
                Some(RunMode::Spawned) => {}
            }
            if chunk.is_empty() {
                return Ok(());
            }
            if !state.receiver_alive || state.producer != ProducerState::Open {
                return Err(TryPushError::new(TryPushErrorKind::Closed, chunk));
            }
            let Some(next_accepted) = state.accepted_bytes.checked_add(length) else {
                return Err(TryPushError::new(TryPushErrorKind::LengthExceeded, chunk));
            };
            if state
                .total_limit
                .is_some_and(|total_limit| next_accepted > total_limit)
            {
                return Err(TryPushError::new(
                    TryPushErrorKind::TotalLimitExceeded,
                    chunk,
                ));
            }
            if matches!(self.shared.framing, UploadFraming::Fixed(expected) if next_accepted > expected)
            {
                return Err(TryPushError::new(TryPushErrorKind::LengthExceeded, chunk));
            }
        }

        let mut offset = 0;
        while offset < chunk.len() {
            let mut state = lock_unpoisoned(&self.shared.state);
            while state.receiver_alive
                && state.producer == ProducerState::Open
                && state.queued_bytes == self.shared.queue_capacity
            {
                state = self
                    .shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if !state.receiver_alive || state.producer != ProducerState::Open {
                return Err(TryPushError::new(
                    TryPushErrorKind::Closed,
                    chunk[offset..].to_vec(),
                ));
            }
            let available = self.shared.queue_capacity - state.queued_bytes;
            let count = available.min(chunk.len() - offset);
            let accepted = chunk[offset..offset + count].to_vec();
            state.accepted_bytes +=
                u64::try_from(count).expect("validated upload slice length must fit u64");
            state.queued_bytes += count;
            state.queue.push_back(accepted);
            offset += count;
            drop(state);
            self.shared.changed.notify_all();
            self.shared.wake_engine();
        }
        Ok(())
    }

    /// Explicitly and irreversibly finishes this producer.
    ///
    /// A fixed-length body must have accepted exactly its declared byte count. This consumes the
    /// unique sender even when the length check fails.
    pub fn finish(mut self) -> Result<(), UploadFinishError> {
        let mut state = lock_unpoisoned(&self.shared.state);
        let expected = match self.shared.framing {
            UploadFraming::Fixed(length) => Some(length),
            UploadFraming::Chunked => None,
        };
        let accepted = state.accepted_bytes;
        let result = if !state.receiver_alive {
            Err(UploadFinishError::new(
                UploadFinishErrorKind::Closed,
                expected,
                accepted,
            ))
        } else if expected.is_some_and(|length| length != accepted) {
            state.producer = ProducerState::LengthMismatch;
            Err(UploadFinishError::new(
                UploadFinishErrorKind::LengthMismatch,
                expected,
                accepted,
            ))
        } else {
            state.producer = ProducerState::Finished;
            Ok(())
        };
        self.terminated = true;
        drop(state);
        self.shared.changed.notify_all();
        self.shared.wake_engine();
        result
    }

    /// Returns the number of caller bytes accepted by this producer so far.
    #[must_use]
    pub fn accepted_bytes(&self) -> u64 {
        lock_unpoisoned(&self.shared.state).accepted_bytes
    }

    /// Returns the caller bytes currently waiting in this transfer's bounded queue.
    #[must_use]
    pub fn queued_bytes(&self) -> usize {
        lock_unpoisoned(&self.shared.state).queued_bytes
    }
}

impl fmt::Debug for UploadSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UploadSender")
            .field(
                "declared_length",
                &match self.shared.framing {
                    UploadFraming::Fixed(length) => Some(length),
                    UploadFraming::Chunked => None,
                },
            )
            .field("queue_capacity", &self.shared.queue_capacity)
            .field("terminated", &self.terminated)
            .finish_non_exhaustive()
    }
}

impl Drop for UploadSender {
    fn drop(&mut self) {
        if self.terminated {
            return;
        }
        let mut state = lock_unpoisoned(&self.shared.state);
        if state.producer == ProducerState::Open {
            state.producer = ProducerState::Abandoned;
        }
        drop(state);
        self.shared.changed.notify_all();
        self.shared.wake_engine();
    }
}

/// Why a nonblocking streamed-upload push did not accept its chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TryPushErrorKind {
    /// The complete chunk would exceed the transfer's currently available queue space.
    WouldBlock,
    /// The chunk can never fit within this transfer's configured queue capacity.
    ChunkTooLarge,
    /// Accepting the chunk would exceed a fixed body's declared length.
    LengthExceeded,
    /// Accepting the chunk would exceed the Engine-owned total request-body ceiling.
    TotalLimitExceeded,
    /// The receiving body or request is no longer accepting upload data.
    Closed,
    /// Blocking push was called before the upload was submitted to an Engine.
    NotSubmitted,
    /// Blocking push is unavailable for a manually driven Engine.
    WrongMode,
}

/// A failed [`UploadSender::try_push`] or [`UploadSender::push`] operation.
#[derive(Debug)]
pub struct TryPushError {
    kind: TryPushErrorKind,
    chunk: Vec<u8>,
}

impl TryPushError {
    fn new(kind: TryPushErrorKind, chunk: Vec<u8>) -> Self {
        Self { kind, chunk }
    }

    /// Returns the stable reason the chunk was not accepted.
    #[must_use]
    pub fn kind(&self) -> TryPushErrorKind {
        self.kind
    }

    /// Returns the unchanged caller-owned chunk for `try_push`, or the unaccepted suffix for
    /// blocking `push`.
    #[must_use]
    pub fn into_chunk(self) -> Vec<u8> {
        self.chunk
    }
}

impl fmt::Display for TryPushError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            TryPushErrorKind::WouldBlock => "the streamed upload queue is full",
            TryPushErrorKind::ChunkTooLarge => "the chunk is larger than the streamed upload queue",
            TryPushErrorKind::LengthExceeded => {
                "the chunk would exceed the fixed streamed upload length"
            }
            TryPushErrorKind::TotalLimitExceeded => {
                "the chunk would exceed the streamed request body limit"
            }
            TryPushErrorKind::Closed => "the streamed upload is closed",
            TryPushErrorKind::NotSubmitted => {
                "blocking streamed upload push requires successful submission"
            }
            TryPushErrorKind::WrongMode => {
                "blocking streamed upload push is unavailable for a manual Engine"
            }
        };
        formatter.write_str(message)
    }
}

impl StdError for TryPushError {}

/// Why [`UploadSender::finish`] could not finish its body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UploadFinishErrorKind {
    /// A fixed body had not accepted exactly its declared length.
    LengthMismatch,
    /// The receiving body or request was already closed.
    Closed,
}

/// Failure while explicitly finishing a unique upload producer.
#[derive(Debug)]
pub struct UploadFinishError {
    kind: UploadFinishErrorKind,
    expected: Option<u64>,
    accepted: u64,
}

impl UploadFinishError {
    fn new(kind: UploadFinishErrorKind, expected: Option<u64>, accepted: u64) -> Self {
        Self {
            kind,
            expected,
            accepted,
        }
    }

    /// Returns the stable failure reason.
    #[must_use]
    pub fn kind(&self) -> UploadFinishErrorKind {
        self.kind
    }

    /// Returns the fixed declared length, or `None` for an unknown-length body.
    #[must_use]
    pub fn expected_bytes(&self) -> Option<u64> {
        self.expected
    }

    /// Returns the caller bytes accepted before finish.
    #[must_use]
    pub fn accepted_bytes(&self) -> u64 {
        self.accepted
    }
}

impl fmt::Display for UploadFinishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            UploadFinishErrorKind::LengthMismatch => write!(
                formatter,
                "fixed streamed upload accepted {} bytes but required {}",
                self.accepted,
                self.expected.unwrap_or(0)
            ),
            UploadFinishErrorKind::Closed => formatter.write_str("the streamed upload is closed"),
        }
    }
}

impl StdError for UploadFinishError {}

/// The final HTTP status and headers for a streamed response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseHead {
    status: u16,
    headers: Vec<Header>,
}

impl ResponseHead {
    #[allow(dead_code)] // Constructed by native HTTP beginning with the submission slice.
    pub(crate) fn new(status: u16, headers: Vec<Header>) -> Self {
        Self { status, headers }
    }

    /// Returns the final HTTP status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Returns the final response headers.
    ///
    /// Informational heads and validated HTTP/1.1 trailers do not appear here.
    #[must_use]
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }
}

/// The result of one nonblocking [`ResponseReader::try_read`] call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StreamRead {
    /// No response-body bytes or terminal state are currently available.
    Pending,
    /// The supplied destination received this many bytes.
    Data(usize),
    /// The complete response body has been consumed successfully.
    Eof,
}

/// A terminal request failure, cancellation, or invalid local streaming operation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum StreamError {
    /// The accepted streaming request failed.
    Failed(Error),
    /// Explicit cancellation won the request's terminal race.
    Cancelled,
    /// The requested reader operation is invalid in its current mode or state.
    Operation(Error),
}

impl StreamError {
    fn operation(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::Operation(Error::new(kind, message))
    }

    /// Returns the underlying NBReq error for failures and invalid operations.
    #[must_use]
    pub fn error(&self) -> Option<&Error> {
        match self {
            Self::Failed(error) | Self::Operation(error) => Some(error),
            Self::Cancelled => None,
        }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(error) => write!(formatter, "streaming request failed: {error}"),
            Self::Cancelled => formatter.write_str("streaming request was cancelled"),
            Self::Operation(error) => write!(formatter, "streaming operation failed: {error}"),
        }
    }
}

impl StdError for StreamError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.error().map(|error| error as &dyn StdError)
    }
}

/// The unique body and terminal consumer for one accepted streaming request.
///
/// The reader is `Send`, deliberately not `Clone` or `Sync`, and owns no hidden Engine. Its
/// cloneable [`RequestHandle`] is cancellation-only. Dropping before known EOF requests
/// cancellation; dropping after the final byte or a no-body final head is harmless.
///
/// ```compile_fail
/// fn require_sync<T: Sync>() {}
/// require_sync::<nbreq::ResponseReader>();
/// ```
pub struct ResponseReader {
    handle: RequestHandle,
    shared: Arc<ResponseShared>,
    run_mode: RunMode,
    head: Option<ResponseHead>,
    current: Option<(Vec<u8>, usize)>,
    body_started: bool,
    eof_reached: bool,
    seen_generation: u64,
    not_sync: PhantomData<Cell<()>>,
}

impl ResponseReader {
    /// Returns an independent cancellation-only handle for this request.
    #[must_use]
    pub fn handle(&self) -> RequestHandle {
        self.handle.clone()
    }

    /// Returns the final response head when available without blocking.
    pub fn try_head(&mut self) -> Result<Option<&ResponseHead>, StreamError> {
        if self.head.is_none() {
            let state = lock_unpoisoned(&self.shared.state);
            self.seen_generation = state.generation;
            if let Some(head) = &state.head {
                self.head = Some(head.clone());
                if state.no_body && state.terminal == Some(StreamTerminal::Complete) {
                    self.eof_reached = true;
                }
            } else if let Some(terminal) = &state.terminal {
                return Err(terminal_error_without_head(terminal));
            }
        }
        Ok(self.head.as_ref())
    }

    /// Waits for and returns the final response head on a spawned Engine.
    ///
    /// Manual Engines return `WrongMode`; this method never drives an Engine internally.
    pub fn wait_head(&mut self) -> Result<&ResponseHead, StreamError> {
        self.require_spawned("wait_head")?;
        while self.head.is_none() {
            if self.try_head()?.is_some() {
                break;
            }
            self.wait_for_change();
        }
        self.head.as_ref().ok_or_else(|| {
            StreamError::operation(
                ErrorKind::Internal,
                "response head wait completed without a head",
            )
        })
    }

    /// Attempts to read available response-body bytes without blocking.
    pub fn try_read(&mut self, destination: &mut [u8]) -> Result<StreamRead, StreamError> {
        if self.try_head()?.is_none() {
            return Ok(StreamRead::Pending);
        }
        if self.eof_reached {
            return Ok(StreamRead::Eof);
        }
        if destination.is_empty() {
            return Ok(StreamRead::Data(0));
        }

        let mut state = lock_unpoisoned(&self.shared.state);
        self.seen_generation = state.generation;
        match &state.terminal {
            Some(StreamTerminal::Failed(error)) => {
                self.current = None;
                return Err(StreamError::Failed(error.clone()));
            }
            Some(StreamTerminal::Cancelled) => {
                self.current = None;
                return Err(StreamError::Cancelled);
            }
            Some(StreamTerminal::Complete) | None => {}
        }
        if self.current.is_none() {
            if let Some(chunk) = state.queue.pop_front() {
                self.current = Some((chunk, 0));
            } else if state.terminal == Some(StreamTerminal::Complete) {
                self.eof_reached = true;
                return Ok(StreamRead::Eof);
            } else {
                return Ok(StreamRead::Pending);
            }
        }

        let (chunk, offset) = self.current.as_mut().ok_or_else(|| {
            StreamError::operation(
                ErrorKind::Internal,
                "response queue yielded no readable chunk",
            )
        })?;
        let count = destination.len().min(chunk.len() - *offset);
        destination[..count].copy_from_slice(&chunk[*offset..*offset + count]);
        *offset += count;
        self.body_started |= count != 0;

        state.queued_bytes -= count;
        state.generation = state.generation.wrapping_add(1);
        self.seen_generation = state.generation;
        let chunk_done = *offset == chunk.len();
        if chunk_done {
            self.current = None;
        }
        if chunk_done && state.queue.is_empty() && state.terminal == Some(StreamTerminal::Complete)
        {
            self.eof_reached = true;
        }
        drop(state);
        self.shared.changed.notify_all();
        self.shared.wake_transport();
        Ok(StreamRead::Data(count))
    }

    /// Reads response-body bytes on a spawned Engine, returning `None` at successful EOF.
    ///
    /// Manual Engines return `WrongMode`; this method never drives an Engine internally.
    pub fn read(&mut self, destination: &mut [u8]) -> Result<Option<usize>, StreamError> {
        self.require_spawned("read")?;
        loop {
            match self.try_read(destination)? {
                StreamRead::Pending => self.wait_for_change(),
                StreamRead::Data(count) => return Ok(Some(count)),
                StreamRead::Eof => return Ok(None),
            }
        }
    }

    /// Consumes an untouched spawned-mode reader and collects its complete bounded response.
    ///
    /// This fails closed after any body byte has already been returned. It is consumer-side sugar,
    /// not a second waiter and not a streaming [`Completion`](crate::Completion).
    pub fn collect(mut self) -> Result<Response, StreamError> {
        self.require_spawned("collect")?;
        if self.body_started {
            return Err(StreamError::operation(
                ErrorKind::InvalidRequest,
                "collect cannot follow a partial streaming body read",
            ));
        }
        let head = self.wait_head()?.clone();
        let mut body = Vec::new();
        let mut buffer = vec![0_u8; 16 * 1024];
        while let Some(count) = self.read(&mut buffer)? {
            body.extend_from_slice(&buffer[..count]);
        }
        Ok(Response::new(head.status, head.headers, body))
    }

    /// Returns whether successful EOF is already known locally.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.eof_reached
    }

    #[cfg(all(test, feature = "native"))]
    pub(crate) fn queued_bytes_for_test(&self) -> usize {
        lock_unpoisoned(&self.shared.state).queued_bytes
    }

    fn require_spawned(&self, operation: &str) -> Result<(), StreamError> {
        if self.run_mode == RunMode::Manual {
            return Err(StreamError::operation(
                ErrorKind::WrongMode,
                format!("{operation} is not available for a manual Engine"),
            ));
        }
        Ok(())
    }

    fn wait_for_change(&mut self) {
        let state = lock_unpoisoned(&self.shared.state);
        let state = self
            .shared
            .changed
            .wait_while(state, |state| state.generation == self.seen_generation)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.seen_generation = state.generation;
    }
}

impl fmt::Debug for ResponseReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseReader")
            .field("request_id", &self.handle.id())
            .field("has_head", &self.head.is_some())
            .field("body_started", &self.body_started)
            .field("eof_reached", &self.eof_reached)
            .finish_non_exhaustive()
    }
}

impl Drop for ResponseReader {
    fn drop(&mut self) {
        let mut state = lock_unpoisoned(&self.shared.state);
        state.reader_alive = false;
        state.queue.clear();
        state.queued_bytes = 0;
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.shared.changed.notify_all();
        self.shared.wake_transport();
        if !self.eof_reached {
            let _cancel_result = self.handle.cancel();
        }
    }
}

fn terminal_error_without_head(terminal: &StreamTerminal) -> StreamError {
    match terminal {
        StreamTerminal::Failed(error) => StreamError::Failed(error.clone()),
        StreamTerminal::Cancelled => StreamError::Cancelled,
        StreamTerminal::Complete => StreamError::operation(
            ErrorKind::Internal,
            "streaming response completed without a final head",
        ),
    }
}

#[allow(dead_code)] // Fed by native HTTP beginning with the submission slice.
pub(crate) struct ResponseSink {
    shared: Arc<ResponseShared>,
    terminal: bool,
}

#[derive(Clone)]
pub(crate) struct ResponseControl {
    shared: Arc<ResponseShared>,
}

impl ResponseControl {
    pub(crate) fn is_terminal(&self) -> bool {
        lock_unpoisoned(&self.shared.state).terminal.is_some()
    }

    pub(crate) fn outcome(&self) -> Option<StreamOutcome> {
        lock_unpoisoned(&self.shared.state)
            .terminal
            .as_ref()
            .map(StreamOutcome::from)
    }

    pub(crate) fn fail(&self, error: Error) -> bool {
        self.commit_terminal(StreamTerminal::Failed(error))
    }

    pub(crate) fn cancel(&self) -> bool {
        self.commit_terminal(StreamTerminal::Cancelled)
    }

    fn commit_terminal(&self, terminal: StreamTerminal) -> bool {
        let mut state = lock_unpoisoned(&self.shared.state);
        if state.terminal.is_some() {
            return false;
        }
        state.queue.clear();
        state.queued_bytes = 0;
        state.terminal = Some(terminal);
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.shared.changed.notify_all();
        self.shared.wake_transport();
        true
    }
}

#[allow(dead_code)]
impl ResponseSink {
    pub(crate) fn publish_head(&mut self, head: ResponseHead, no_body: bool) -> bool {
        let mut state = lock_unpoisoned(&self.shared.state);
        if state.head.is_some() || state.terminal.is_some() || !state.reader_alive {
            return false;
        }
        state.head = Some(head);
        state.no_body = no_body;
        if no_body {
            state.terminal = Some(StreamTerminal::Complete);
            self.terminal = true;
        }
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.shared.changed.notify_all();
        true
    }

    pub(crate) fn try_push(&mut self, chunk: Vec<u8>) -> Result<(), ResponsePushError> {
        let mut state = lock_unpoisoned(&self.shared.state);
        if !state.reader_alive || state.terminal.is_some() {
            return Err(ResponsePushError::Closed(chunk));
        }
        if state.head.is_none() {
            return Err(ResponsePushError::Protocol(chunk));
        }
        let Some(next_total) = state.received_bytes.checked_add(chunk.len()) else {
            return Err(ResponsePushError::Limit(chunk));
        };
        if next_total > self.shared.total_limit {
            return Err(ResponsePushError::Limit(chunk));
        }
        if chunk.len() > self.shared.queue_capacity - state.queued_bytes {
            return Err(ResponsePushError::WouldBlock(chunk));
        }
        state.received_bytes = next_total;
        state.queued_bytes += chunk.len();
        if !chunk.is_empty() {
            state.queue.push_back(chunk);
        }
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.shared.changed.notify_all();
        Ok(())
    }

    pub(crate) fn complete(&mut self) {
        self.commit_terminal(StreamTerminal::Complete);
    }

    pub(crate) fn fail(&mut self, error: Error) {
        self.commit_terminal(StreamTerminal::Failed(error));
    }

    pub(crate) fn cancel(&mut self) {
        self.commit_terminal(StreamTerminal::Cancelled);
    }

    pub(crate) fn available_capacity(&self) -> usize {
        let state = lock_unpoisoned(&self.shared.state);
        self.shared.queue_capacity - state.queued_bytes
    }

    pub(crate) fn reader_alive(&self) -> bool {
        lock_unpoisoned(&self.shared.state).reader_alive
    }

    fn commit_terminal(&mut self, terminal: StreamTerminal) {
        let mut state = lock_unpoisoned(&self.shared.state);
        if state.terminal.is_some() {
            return;
        }
        if !matches!(terminal, StreamTerminal::Complete) {
            state.queue.clear();
            state.queued_bytes = 0;
        }
        state.terminal = Some(terminal);
        state.generation = state.generation.wrapping_add(1);
        self.terminal = true;
        drop(state);
        self.shared.changed.notify_all();
    }
}

impl Drop for ResponseSink {
    fn drop(&mut self) {
        if !self.terminal {
            self.fail(Error::new(
                ErrorKind::Internal,
                "streaming response producer ended without a terminal result",
            ));
        }
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum ResponsePushError {
    WouldBlock(Vec<u8>),
    Closed(Vec<u8>),
    Protocol(Vec<u8>),
    Limit(Vec<u8>),
}

#[allow(dead_code)]
impl ResponsePushError {
    pub(crate) fn into_chunk(self) -> Vec<u8> {
        match self {
            Self::WouldBlock(chunk)
            | Self::Closed(chunk)
            | Self::Protocol(chunk)
            | Self::Limit(chunk) => chunk,
        }
    }
}

struct ResponseShared {
    state: Mutex<ResponseState>,
    changed: Condvar,
    transport_waker: Option<StreamWaker>,
    queue_capacity: usize,
    total_limit: usize,
}

impl ResponseShared {
    fn wake_transport(&self) {
        if let Some(waker) = &self.transport_waker {
            waker();
        }
    }
}

struct ResponseState {
    head: Option<ResponseHead>,
    no_body: bool,
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    received_bytes: usize,
    terminal: Option<StreamTerminal>,
    reader_alive: bool,
    generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StreamTerminal {
    Complete,
    Failed(Error),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StreamOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl From<&StreamTerminal> for StreamOutcome {
    fn from(terminal: &StreamTerminal) -> Self {
        match terminal {
            StreamTerminal::Complete => Self::Completed,
            StreamTerminal::Failed(_) => Self::Failed,
            StreamTerminal::Cancelled => Self::Cancelled,
        }
    }
}

#[allow(dead_code)] // Called by stream acceptance beginning with the next slice.
pub(crate) fn response_pair(
    handle: RequestHandle,
    run_mode: RunMode,
    queue_capacity: usize,
    total_limit: usize,
    transport_waker: Option<StreamWaker>,
) -> Result<(ResponseReader, ResponseSink, ResponseControl), Error> {
    if queue_capacity == 0 {
        return Err(Error::limit(
            LimitKind::StreamingQueueBytes,
            "a streaming response queue capacity must be greater than zero",
        ));
    }
    let shared = Arc::new(ResponseShared {
        state: Mutex::new(ResponseState {
            head: None,
            no_body: false,
            queue: VecDeque::new(),
            queued_bytes: 0,
            received_bytes: 0,
            terminal: None,
            reader_alive: true,
            generation: 0,
        }),
        changed: Condvar::new(),
        transport_waker,
        queue_capacity,
        total_limit,
    });
    Ok((
        ResponseReader {
            handle,
            shared: Arc::clone(&shared),
            run_mode,
            head: None,
            current: None,
            body_started: false,
            eof_reached: false,
            seen_generation: 0,
            not_sync: PhantomData,
        },
        ResponseSink {
            shared: Arc::clone(&shared),
            terminal: false,
        },
        ResponseControl { shared },
    ))
}

/// A request whose response body will be consumed through the streaming API.
///
/// A StreamRequest can carry either replayable buffered bytes or one unique streamed upload. It is
/// intentionally not cloneable. Submit it with [`Client::submit_stream`](crate::Client::submit_stream);
/// buffered Client methods deliberately continue to accept only [`Request`].
///
/// ```compile_fail
/// fn require_sync<T: Sync>() {}
/// require_sync::<nbreq::StreamRequest>();
/// ```
#[derive(Debug)]
pub struct StreamRequest {
    request: Request,
    stream_body: Option<UploadBody>,
}

impl StreamRequest {
    /// Starts a streaming-response request builder.
    #[must_use]
    pub fn builder(method: Method, url: impl Into<String>) -> StreamRequestBuilder {
        StreamRequestBuilder::new(Request::builder(method, url))
    }

    /// Starts a streaming-response GET request.
    #[must_use]
    pub fn get(url: impl Into<String>) -> StreamRequestBuilder {
        Self::builder(Method::Get, url)
    }

    /// Starts a streaming-response POST request.
    #[must_use]
    pub fn post(url: impl Into<String>) -> StreamRequestBuilder {
        Self::builder(Method::Post, url)
    }

    /// Starts a streaming-response HEAD request.
    #[must_use]
    pub fn head(url: impl Into<String>) -> StreamRequestBuilder {
        Self::builder(Method::Head, url)
    }

    /// Starts a streaming-response PUT request.
    #[must_use]
    pub fn put(url: impl Into<String>) -> StreamRequestBuilder {
        Self::builder(Method::Put, url)
    }

    /// Starts a streaming-response PATCH request.
    #[must_use]
    pub fn patch(url: impl Into<String>) -> StreamRequestBuilder {
        Self::builder(Method::Patch, url)
    }

    /// Starts a streaming-response DELETE request.
    #[must_use]
    pub fn delete(url: impl Into<String>) -> StreamRequestBuilder {
        Self::builder(Method::Delete, url)
    }

    /// Returns the HTTP method.
    #[must_use]
    pub fn method(&self) -> &Method {
        self.request.method()
    }

    /// Returns the URL text.
    #[must_use]
    pub fn url(&self) -> &str {
        self.request.url()
    }

    /// Returns the request headers.
    #[must_use]
    pub fn headers(&self) -> &[Header] {
        self.request.headers()
    }

    /// Returns the buffered upload bytes, or `None` when this request owns an UploadBody.
    #[must_use]
    pub fn body(&self) -> Option<&[u8]> {
        self.stream_body.is_none().then(|| self.request.body())
    }

    /// Returns whether this request owns a unique streamed upload body.
    #[must_use]
    pub fn has_stream_body(&self) -> bool {
        self.stream_body.is_some()
    }

    /// Returns the portable timeout and redirect options.
    #[must_use]
    pub fn options(&self) -> &RequestOptions {
        self.request.options()
    }

    #[allow(dead_code)] // Used by the submission seam in the next WP9.4 slice.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.request.validate()?;
        if let Some(body) = &self.stream_body {
            validate_stream_body_request(&self.request, body)?;
        }
        Ok(())
    }

    pub(crate) fn request(&self) -> &Request {
        &self.request
    }

    pub(crate) fn upload_queue_capacity(&self) -> usize {
        self.stream_body
            .as_ref()
            .map_or(0, UploadBody::queue_capacity)
    }

    pub(crate) fn bind_upload(
        &self,
        max_queue_capacity: usize,
        total_limit: usize,
        run_mode: RunMode,
        engine_waker: StreamWaker,
    ) -> Result<(), Error> {
        if let Some(body) = &self.stream_body {
            body.bind(max_queue_capacity, total_limit, run_mode, engine_waker)?;
        }
        Ok(())
    }

    #[allow(dead_code)] // Consumed when the native wire pump lands after registry submission.
    pub(crate) fn into_parts(self) -> (Request, Option<UploadBody>) {
        (self.request, self.stream_body)
    }
}

impl From<Request> for StreamRequest {
    fn from(request: Request) -> Self {
        Self {
            request,
            stream_body: None,
        }
    }
}

/// Builder for a [`StreamRequest`].
#[derive(Debug)]
pub struct StreamRequestBuilder {
    request: RequestBuilder,
    buffered_body_selected: bool,
    stream_body: Option<UploadBody>,
    body_conflict: Option<BodyConflict>,
}

impl StreamRequestBuilder {
    fn new(request: RequestBuilder) -> Self {
        Self {
            request,
            buffered_body_selected: false,
            stream_body: None,
            body_conflict: None,
        }
    }

    /// Adds an owned header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.request = self.request.header(name, value);
        self
    }

    /// Selects a replayable buffered upload body.
    ///
    /// Combining this with [`Self::body_stream`] is rejected by [`Self::build`].
    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        if self.stream_body.is_some() {
            self.body_conflict = Some(BodyConflict::MixedModes);
        }
        self.buffered_body_selected = true;
        self.request = self.request.body(body);
        self
    }

    /// Selects one unique, non-replayable streamed upload body.
    ///
    /// Combining this with [`Self::body`] or another streamed body is rejected by [`Self::build`].
    #[must_use]
    pub fn body_stream(mut self, body: UploadBody) -> Self {
        if self.buffered_body_selected {
            self.body_conflict = Some(BodyConflict::MixedModes);
        } else if self.stream_body.is_some() {
            self.body_conflict = Some(BodyConflict::MultipleStreams);
        }
        if self.stream_body.is_none() {
            self.stream_body = Some(body);
        }
        self
    }

    /// Sets the portable timeout options.
    #[must_use]
    pub fn options(mut self, options: RequestOptions) -> Self {
        self.request = self.request.options(options);
        self
    }

    /// Sets the maximum connection-establishment duration.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.request = self.request.connect_timeout(timeout);
        self
    }

    /// Sets the maximum duration without useful I/O progress.
    #[must_use]
    pub fn inactivity_timeout(mut self, timeout: Duration) -> Self {
        self.request = self.request.inactivity_timeout(timeout);
        self
    }

    /// Sets the maximum total duration beginning at request acceptance.
    #[must_use]
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        self.request = self.request.total_timeout(timeout);
        self
    }

    /// Sets the maximum redirects followed for replayable buffered uploads.
    ///
    /// A streamed upload returns every redirect response unfollowed regardless of this value.
    #[must_use]
    pub fn redirect_limit(mut self, limit: u8) -> Self {
        self.request = self.request.redirect_limit(limit);
        self
    }

    /// Sets HTTPS certificate and hostname verification policy.
    #[must_use]
    pub fn tls_verification(mut self, verification: TlsVerification) -> Self {
        self.request = self.request.tls_verification(verification);
        self
    }

    /// Validates the backend-independent streaming request contract.
    pub fn build(self) -> Result<StreamRequest, Error> {
        if let Some(conflict) = self.body_conflict {
            let message = match conflict {
                BodyConflict::MixedModes => {
                    "a StreamRequest cannot contain both buffered and streamed upload bodies"
                }
                BodyConflict::MultipleStreams => {
                    "a StreamRequest cannot contain more than one streamed upload body"
                }
            };
            return Err(Error::new(ErrorKind::InvalidRequest, message));
        }
        let request = self.request.build()?;
        if let Some(body) = &self.stream_body {
            validate_stream_body_request(&request, body)?;
        }
        Ok(StreamRequest {
            request,
            stream_body: self.stream_body,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum BodyConflict {
    MixedModes,
    MultipleStreams,
}

fn validate_stream_body_request(request: &Request, body: &UploadBody) -> Result<(), Error> {
    if matches!(request.method(), Method::Get | Method::Head) {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "GET and HEAD cannot carry a streamed upload body",
        ));
    }
    if request.headers().iter().any(|header| {
        header.name().eq_ignore_ascii_case("content-length")
            || header.name().eq_ignore_ascii_case("transfer-encoding")
    }) {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "NBReq generates framing headers for a streamed upload body",
        ));
    }
    if request
        .headers()
        .iter()
        .any(|header| header.name().eq_ignore_ascii_case("expect"))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "Expect is not supported for a streamed upload body",
        ));
    }
    body.validate_for_build()
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum UploadFraming {
    Fixed(u64),
    Chunked,
}

struct UploadShared {
    state: Mutex<UploadState>,
    changed: Condvar,
    engine_waker: Mutex<Option<StreamWaker>>,
    framing: UploadFraming,
    queue_capacity: usize,
}

impl UploadShared {
    fn wake_engine(&self) {
        if let Some(waker) = lock_unpoisoned(&self.engine_waker).clone() {
            waker();
        }
    }
}

struct UploadState {
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    accepted_bytes: u64,
    producer: ProducerState,
    receiver_alive: bool,
    total_limit: Option<u64>,
    run_mode: Option<RunMode>,
}

#[allow(dead_code)] // Consumed by native HTTP beginning with the next submission slice.
pub(crate) enum UploadPoll {
    Chunk(Vec<u8>),
    Pending,
    Finished,
    Failed(Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProducerState {
    Open,
    Finished,
    Abandoned,
    LengthMismatch,
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn consume_one(body: &UploadBody) -> Vec<u8> {
        let mut state = lock_unpoisoned(&body.shared.state);
        let chunk = state.queue.pop_front().expect("one queued chunk");
        state.queued_bytes -= chunk.len();
        chunk
    }

    #[test]
    fn fixed_pair_is_unique_bounded_and_length_checked_without_eating_chunks() {
        let (body, mut sender) = UploadBody::fixed(6, 4).expect("valid fixed pair");
        sender.try_push(b"abc".to_vec()).expect("first chunk fits");

        let error = sender
            .try_push(b"de".to_vec())
            .expect_err("whole second chunk does not currently fit");
        assert_eq!(error.kind(), TryPushErrorKind::WouldBlock);
        assert_eq!(error.into_chunk(), b"de");

        assert_eq!(consume_one(&body), b"abc");
        sender
            .try_push(b"def".to_vec())
            .expect("space was released");
        let error = sender
            .try_push(b"g".to_vec())
            .expect_err("declared length is strict");
        assert_eq!(error.kind(), TryPushErrorKind::LengthExceeded);
        assert_eq!(error.into_chunk(), b"g");
        sender.finish().expect("exact fixed length finishes");
    }

    #[test]
    fn oversized_chunk_and_short_finish_fail_closed() {
        let (body, mut sender) = UploadBody::fixed(5, 4).expect("valid fixed pair");
        let error = sender
            .try_push(b"12345".to_vec())
            .expect_err("a chunk larger than the window can never fit");
        assert_eq!(error.kind(), TryPushErrorKind::ChunkTooLarge);
        assert_eq!(error.into_chunk(), b"12345");

        sender
            .try_push(b"1234".to_vec())
            .expect("partial body fits");
        let error = sender.finish().expect_err("short body cannot finish");
        assert_eq!(error.kind(), UploadFinishErrorKind::LengthMismatch);
        assert_eq!(error.expected_bytes(), Some(5));
        assert_eq!(error.accepted_bytes(), 4);

        let error = StreamRequest::post("http://example.test/")
            .body_stream(body)
            .build()
            .expect_err("a mismatched producer poisons later construction");
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
    }

    #[test]
    fn chunked_body_finishes_without_a_declared_length() {
        let (body, mut sender) = UploadBody::chunked(8).expect("valid chunked pair");
        assert_eq!(body.declared_length(), None);
        sender.try_push(b"audio".to_vec()).expect("chunk fits");
        sender
            .finish()
            .expect("unknown-length body finishes explicitly");

        let request = StreamRequest::post("https://example.test/audio")
            .body_stream(body)
            .total_timeout(Duration::from_secs(30))
            .build()
            .expect("finished chunked body is still a valid request");
        assert!(request.has_stream_body());
        assert_eq!(request.body(), None);
    }

    #[test]
    fn stream_request_has_a_real_buffered_builder_and_request_sugar() {
        let request = StreamRequest::post("https://example.test/")
            .header("Content-Type", "application/json")
            .body(br#"{"ok":true}"#.to_vec())
            .redirect_limit(2)
            .build()
            .expect("buffered streaming-response request builds");
        assert_eq!(request.body(), Some(br#"{"ok":true}"#.as_slice()));
        assert!(!request.has_stream_body());

        let ordinary = Request::get("https://example.test/")
            .build()
            .expect("ordinary request builds");
        let converted = StreamRequest::from(ordinary);
        assert_eq!(converted.method(), &Method::Get);
        assert_eq!(converted.body(), Some([].as_slice()));
    }

    #[test]
    fn mixed_body_modes_and_stream_framing_headers_fail_at_build() {
        let (body, _sender) = UploadBody::fixed(3, 4).expect("valid fixed pair");
        let error = StreamRequest::post("http://example.test/")
            .body(b"abc".to_vec())
            .body_stream(body)
            .build()
            .expect_err("body modes cannot silently replace one another");
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);

        let (first, _first_sender) = UploadBody::chunked(4).expect("first stream pair");
        let (second, _second_sender) = UploadBody::chunked(4).expect("second stream pair");
        let error = StreamRequest::post("http://example.test/")
            .body_stream(first)
            .body_stream(second)
            .build()
            .expect_err("two unique stream bodies cannot share one request");
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        assert_eq!(
            error.message(),
            "a StreamRequest cannot contain more than one streamed upload body"
        );

        for header in ["Content-Length", "Transfer-Encoding", "Expect"] {
            let (body, _sender) = UploadBody::chunked(4).expect("valid chunked pair");
            let error = StreamRequest::post("http://example.test/")
                .header(header, "value")
                .body_stream(body)
                .build()
                .expect_err("NBReq owns streamed framing policy");
            assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        }
    }

    #[test]
    fn get_head_abandoned_sender_and_zero_capacity_fail_closed() {
        assert_eq!(
            UploadBody::chunked(0)
                .expect_err("zero capacity is not a viable queue")
                .kind(),
            ErrorKind::InvalidRequest
        );

        for builder in [
            StreamRequest::get("http://example.test/"),
            StreamRequest::head("http://example.test/"),
        ] {
            let (body, _sender) = UploadBody::chunked(4).expect("valid chunked pair");
            let error = builder
                .body_stream(body)
                .build()
                .expect_err("GET and HEAD streamed uploads are forbidden");
            assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        }

        let (body, sender) = UploadBody::chunked(4).expect("valid chunked pair");
        drop(sender);
        let error = StreamRequest::post("http://example.test/")
            .body_stream(body)
            .build()
            .expect_err("abandoned producer cannot become a request");
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
    }

    #[test]
    fn dropping_body_wakes_sender_as_closed_and_returns_the_chunk() {
        let (body, mut sender) = UploadBody::chunked(4).expect("valid chunked pair");
        drop(body);
        let error = sender
            .try_push(b"data".to_vec())
            .expect_err("a sender cannot outlive its body");
        assert_eq!(error.kind(), TryPushErrorKind::Closed);
        assert_eq!(error.into_chunk(), b"data");
    }

    #[test]
    fn blocking_push_requires_a_spawned_engine_binding() {
        let (body, mut sender) = UploadBody::chunked(4).expect("unbound pair must construct");
        let error = sender
            .push(b"data".to_vec())
            .expect_err("blocking push before submission must fail");
        assert_eq!(error.kind(), TryPushErrorKind::NotSubmitted);
        assert_eq!(error.into_chunk(), b"data");
        drop(body);

        let (body, mut sender) = UploadBody::chunked(4).expect("manual pair must construct");
        body.bind(4, 16, RunMode::Manual, Arc::new(|| {}))
            .expect("manual body must bind");
        let error = sender
            .push(b"data".to_vec())
            .expect_err("manual blocking push must fail");
        assert_eq!(error.kind(), TryPushErrorKind::WrongMode);
        assert_eq!(error.into_chunk(), b"data");
    }

    #[test]
    fn engine_pull_binding_revalidates_limits_and_observes_producer_terminal_state() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_count = Arc::clone(&wakes);
        let waker: StreamWaker = Arc::new(move || {
            wake_count.fetch_add(1, Ordering::Relaxed);
        });
        let (mut body, mut sender) = UploadBody::fixed(5, 4).expect("valid fixed pair");
        body.bind(4, 5, RunMode::Spawned, waker)
            .expect("body binds once");
        sender.try_push(b"abc".to_vec()).expect("first chunk fits");
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        match body.try_pop() {
            UploadPoll::Chunk(chunk) => assert_eq!(chunk, b"abc"),
            _ => panic!("Engine pull must receive the queued chunk"),
        }
        sender
            .try_push(b"de".to_vec())
            .expect("remaining bytes fit");
        sender.finish().expect("exact body finishes");
        match body.try_pop() {
            UploadPoll::Chunk(chunk) => assert_eq!(chunk, b"de"),
            _ => panic!("Engine pull must receive the final chunk"),
        }
        assert!(matches!(body.try_pop(), UploadPoll::Finished));

        let (body, mut sender) = UploadBody::chunked(4).expect("valid chunked pair");
        body.bind(4, 3, RunMode::Spawned, Arc::new(|| {}))
            .expect("queue binds beneath total limit");
        let error = sender
            .try_push(b"four".to_vec())
            .expect_err("bound total ceiling is enforced after acceptance");
        assert_eq!(error.kind(), TryPushErrorKind::TotalLimitExceeded);
        assert_eq!(error.into_chunk(), b"four");

        let (body, _sender) = UploadBody::chunked(8).expect("caller-owned queue may be larger");
        let error = body
            .bind(4, 16, RunMode::Spawned, Arc::new(|| {}))
            .expect_err("Engine clamps the accepted transfer window");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.limit_kind(), Some(LimitKind::StreamingQueueBytes));
    }

    #[test]
    fn bind_rechecks_sender_abandonment_as_a_send_failure_and_close_wakes_the_sender() {
        let (body, sender) = UploadBody::chunked(4).expect("valid chunked pair");
        body.validate_for_build()
            .expect("producer is alive during construction");
        drop(sender);
        let error = body
            .bind(4, 16, RunMode::Spawned, Arc::new(|| {}))
            .expect_err("submit-time validation must observe later abandonment");
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert_eq!(error.transport_stage(), Some(crate::TransportStage::Send));

        let (mut body, mut sender) = UploadBody::chunked(4).expect("valid chunked pair");
        sender
            .try_push(b"old".to_vec())
            .expect("queue accepts data");
        body.close();
        assert!(matches!(body.try_pop(), UploadPoll::Failed(_)));
        let error = sender
            .try_push(b"new".to_vec())
            .expect_err("closed Engine receiver rejects more data");
        assert_eq!(error.kind(), TryPushErrorKind::Closed);
        assert_eq!(error.into_chunk(), b"new");
    }

    #[test]
    fn finish_and_abandon_wake_a_bound_engine() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let wakes = Arc::new(AtomicUsize::new(0));
        let waker: StreamWaker = {
            let wakes = Arc::clone(&wakes);
            Arc::new(move || {
                wakes.fetch_add(1, Ordering::Relaxed);
            })
        };
        let (body, sender) = UploadBody::chunked(8).expect("valid chunked pair");
        body.bind(8, 16, RunMode::Spawned, Arc::clone(&waker))
            .expect("body must bind");
        sender.finish().expect("chunked sender must finish");
        assert_eq!(wakes.load(Ordering::Relaxed), 1);

        let (body, sender) = UploadBody::chunked(8).expect("valid chunked pair");
        body.bind(8, 16, RunMode::Spawned, waker)
            .expect("body must bind");
        drop(sender);
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    fn synthetic_response_pair(
        run_mode: RunMode,
        queue_capacity: usize,
        total_limit: usize,
    ) -> (
        crate::Engine,
        crate::testing::TestController,
        RequestHandle,
        ResponseReader,
        ResponseSink,
    ) {
        let config = match run_mode {
            RunMode::Spawned => crate::EngineConfig::spawned(),
            RunMode::Manual => crate::EngineConfig::manual(),
        };
        let (engine, controller) =
            crate::testing::engine(config).expect("held Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get("http://example.test/")
                    .build()
                    .expect("synthetic request must build"),
            )
            .expect("synthetic request must submit");
        let handle = pending.handle();
        drop(pending);
        let (reader, sink, _control) =
            response_pair(handle.clone(), run_mode, queue_capacity, total_limit, None)
                .expect("synthetic response pair must construct");
        (engine, controller, handle, reader, sink)
    }

    #[test]
    fn no_body_head_is_immediate_eof_and_drop_does_not_cancel() {
        let (engine, controller, handle, mut reader, mut sink) =
            synthetic_response_pair(RunMode::Spawned, 8, 16);
        assert!(sink.publish_head(ResponseHead::new(204, Vec::new()), true));
        assert_eq!(reader.wait_head().expect("head must publish").status(), 204);
        assert!(reader.is_eof());
        assert_eq!(reader.read(&mut [0_u8; 1]).expect("EOF must read"), None);
        drop(reader);
        assert_eq!(
            controller.active_requests(),
            1,
            "dropping a known-complete no-body response must not cancel"
        );
        assert!(controller.complete(handle.id(), crate::Completion::Cancelled));
        drop(sink);
        engine.shutdown().expect("synthetic Engine must stop");
    }

    #[test]
    fn collect_returns_an_ordinary_response_but_rejects_prefix_loss() {
        let (engine, controller, handle, reader, mut sink) =
            synthetic_response_pair(RunMode::Spawned, 8, 16);
        assert!(sink.publish_head(
            ResponseHead::new(201, vec![Header::new("X-Test", b"yes".to_vec())]),
            false,
        ));
        sink.try_push(b"body".to_vec()).expect("body fits");
        sink.complete();
        let response = reader.collect().expect("untouched reader collects");
        assert_eq!(response.status(), 201);
        assert_eq!(response.body(), b"body");
        assert_eq!(controller.active_requests(), 1);
        assert!(controller.complete(handle.id(), crate::Completion::Cancelled));
        drop(sink);
        engine.shutdown().expect("synthetic Engine must stop");

        let (engine, controller, _handle, mut reader, mut sink) =
            synthetic_response_pair(RunMode::Spawned, 8, 16);
        assert!(sink.publish_head(ResponseHead::new(200, Vec::new()), false));
        sink.try_push(b"body".to_vec()).expect("body fits");
        sink.complete();
        assert_eq!(
            reader.try_read(&mut [0_u8; 1]).expect("prefix reads"),
            StreamRead::Data(1)
        );
        let error = reader
            .collect()
            .expect_err("collect after a body prefix must fail closed");
        assert_eq!(
            error.error().map(Error::kind),
            Some(ErrorKind::InvalidRequest)
        );
        assert_eq!(
            controller.active_requests(),
            0,
            "failed collect drops the incomplete reader and cancels"
        );
        sink.cancel();
        engine.shutdown().expect("synthetic Engine must stop");
    }

    #[test]
    fn response_queue_backpressures_and_releases_capacity_as_bytes_are_read() {
        let (engine, controller, handle, mut reader, mut sink) =
            synthetic_response_pair(RunMode::Spawned, 4, 8);
        assert!(sink.publish_head(ResponseHead::new(200, Vec::new()), false));
        sink.try_push(b"abcd".to_vec()).expect("window fills");
        let error = sink
            .try_push(b"e".to_vec())
            .expect_err("full response window backpressures");
        assert!(matches!(error, ResponsePushError::WouldBlock(_)));
        assert_eq!(error.into_chunk(), b"e");

        let mut prefix = [0_u8; 2];
        assert_eq!(
            reader.try_read(&mut prefix).expect("prefix reads"),
            StreamRead::Data(2)
        );
        assert_eq!(&prefix, b"ab");
        assert_eq!(sink.available_capacity(), 2);
        sink.try_push(b"ef".to_vec())
            .expect("reader released space");
        sink.complete();

        let mut remainder = Vec::new();
        let mut buffer = [0_u8; 4];
        loop {
            match reader.try_read(&mut buffer).expect("remainder reads") {
                StreamRead::Data(count) => remainder.extend_from_slice(&buffer[..count]),
                StreamRead::Eof => break,
                StreamRead::Pending => panic!("completed response cannot become pending"),
            }
        }
        assert_eq!(remainder, b"cdef");
        drop(reader);
        assert_eq!(controller.active_requests(), 1);
        assert!(controller.complete(handle.id(), crate::Completion::Cancelled));
        drop(sink);
        engine.shutdown().expect("synthetic Engine must stop");
    }

    #[test]
    fn manual_reader_never_blocks_or_drives_and_try_methods_still_progress() {
        let (engine, controller, handle, mut reader, mut sink) =
            synthetic_response_pair(RunMode::Manual, 8, 8);
        let error = reader
            .wait_head()
            .expect_err("manual wait_head must fail explicitly");
        assert_eq!(error.error().map(Error::kind), Some(ErrorKind::WrongMode));
        assert_eq!(reader.try_head().expect("try_head is nonblocking"), None);

        assert!(sink.publish_head(ResponseHead::new(200, Vec::new()), false));
        sink.try_push(b"ok".to_vec()).expect("body fits");
        sink.complete();
        assert_eq!(
            reader
                .try_head()
                .expect("head query succeeds")
                .expect("head is available")
                .status(),
            200
        );
        let error = reader
            .read(&mut [0_u8; 2])
            .expect_err("manual blocking read must fail explicitly");
        assert_eq!(error.error().map(Error::kind), Some(ErrorKind::WrongMode));
        assert_eq!(
            reader
                .try_read(&mut [0_u8; 2])
                .expect("manual try_read progresses"),
            StreamRead::Data(2)
        );
        assert!(reader.is_eof());
        drop(reader);
        assert_eq!(controller.active_requests(), 1);
        assert!(controller.complete(handle.id(), crate::Completion::Cancelled));
        drop(sink);
        engine
            .shutdown()
            .expect("manual synthetic Engine must stop");
    }

    #[test]
    fn reader_drop_before_eof_cancels_and_terminal_before_head_is_observable() {
        let (engine, controller, _handle, reader, mut sink) =
            synthetic_response_pair(RunMode::Spawned, 8, 8);
        drop(reader);
        assert_eq!(controller.active_requests(), 0);
        assert!(!sink.reader_alive());
        sink.cancel();
        engine.shutdown().expect("synthetic Engine must stop");

        let (engine, _controller, _handle, mut reader, mut sink) =
            synthetic_response_pair(RunMode::Spawned, 8, 8);
        sink.fail(Error::transport(
            crate::TransportStage::Receive,
            "synthetic receive failure",
        ));
        let error = reader
            .try_head()
            .expect_err("failure before head must reach reader");
        assert!(matches!(error, StreamError::Failed(_)));
        drop(reader);
        engine.shutdown().expect("synthetic Engine must stop");
    }

    #[test]
    fn failure_discards_a_reader_local_partial_chunk_without_accounting_underflow() {
        let (engine, _controller, _handle, mut reader, mut sink) =
            synthetic_response_pair(RunMode::Spawned, 8, 8);
        assert!(sink.publish_head(ResponseHead::new(200, Vec::new()), false));
        sink.try_push(b"abcd".to_vec()).expect("body fits");
        assert_eq!(
            reader.try_read(&mut [0_u8; 1]).expect("prefix reads"),
            StreamRead::Data(1)
        );
        sink.fail(Error::transport(
            crate::TransportStage::Receive,
            "failure after partial delivery",
        ));
        let error = reader
            .try_read(&mut [0_u8; 3])
            .expect_err("terminal failure must discard the local remainder");
        assert!(matches!(error, StreamError::Failed(_)));
        drop(reader);
        engine.shutdown().expect("synthetic Engine must stop");
    }
}
