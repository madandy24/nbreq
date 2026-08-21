use std::cell::Cell;
use std::collections::VecDeque;
use std::error::Error as StdError;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::{
    Error, ErrorKind, Header, Method, Request, RequestBuilder, RequestOptions, TlsVerification,
};

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
            }),
            changed: Condvar::new(),
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
    /// The receiving body or request is no longer accepting upload data.
    Closed,
}

/// A failed all-or-nothing [`UploadSender::try_push`] operation.
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

    /// Returns the unchanged caller-owned chunk.
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
            TryPushErrorKind::Closed => "the streamed upload is closed",
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

/// A request whose response body will be consumed through the streaming API.
///
/// A StreamRequest can carry either replayable buffered bytes or one unique streamed upload. It is
/// intentionally not cloneable. Submission and `ResponseReader` arrive in the next WP9.4 slice;
/// constructing the type does not make buffered Client methods accept it.
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
    body_conflict: bool,
}

impl StreamRequestBuilder {
    fn new(request: RequestBuilder) -> Self {
        Self {
            request,
            buffered_body_selected: false,
            stream_body: None,
            body_conflict: false,
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
            self.body_conflict = true;
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
        if self.buffered_body_selected || self.stream_body.is_some() {
            self.body_conflict = true;
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
        if self.body_conflict {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "a StreamRequest cannot contain both buffered and streamed upload bodies",
            ));
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
enum UploadFraming {
    Fixed(u64),
    Chunked,
}

struct UploadShared {
    state: Mutex<UploadState>,
    changed: Condvar,
    framing: UploadFraming,
    queue_capacity: usize,
}

struct UploadState {
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    accepted_bytes: u64,
    producer: ProducerState,
    receiver_alive: bool,
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
}
