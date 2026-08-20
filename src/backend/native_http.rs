//! Incremental HTTP/1.1 serialization and response framing for the native backend.
//!
//! `httparse` recognizes response heads and chunk-size lines. NBReq owns request policy, body
//! framing, limits, EOF semantics, and the state transition that eventually releases a socket.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::native::{
    NATIVE_SAFETY_POLL, NativeEvent, NativeFailure, NativeFailureKind, NativeReactor, SlotId,
};
use super::native_dns::{NativeResolver, ResolveKey, ResolverConfig};
use super::{Backend, BackendCompletion, BackendFactory, PollMode};
use crate::registry::Shared;
use crate::types::http_origin;
use crate::{
    Completion, EngineConfig, Error, ErrorKind, Header, LimitKind, Method, Request, RequestId,
    Response, ShutdownError, TimeoutKind, TransportStage,
};

const MAX_INFORMATIONAL_RESPONSES: u8 = 8;

#[derive(Clone, Copy)]
pub(super) struct HttpLimits {
    pub(super) body_bytes: usize,
    pub(super) header_bytes: usize,
    pub(super) header_count: usize,
}

impl HttpLimits {
    fn from_config(config: &EngineConfig) -> Self {
        Self {
            body_bytes: config.max_response_body_bytes(),
            header_bytes: config.max_header_bytes(),
            header_count: config.max_header_count(),
        }
    }

    fn reactor_receive_limit(self) -> usize {
        self.header_bytes
            .saturating_mul(usize::from(MAX_INFORMATIONAL_RESPONSES) + 2)
            .saturating_add(self.body_bytes)
            .saturating_add(16 * 1024)
    }
}

#[derive(Debug)]
pub(super) struct SerializedRequest {
    pub(super) bytes: Vec<u8>,
    pub(super) response_to_head: bool,
}

pub(super) fn serialize_request(
    request: &Request,
    limits: HttpLimits,
) -> Result<SerializedRequest, Error> {
    request.validate()?;
    let target = RequestTarget::parse(request.url())?;
    if !target.scheme.eq_ignore_ascii_case("http") {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "the native cleartext HTTP slice does not yet implement HTTPS",
        ));
    }

    let mut host_count = 0_usize;
    let mut content_length = None;
    let mut has_connection = false;
    for header in request.headers() {
        if header.name().eq_ignore_ascii_case("host") {
            host_count += 1;
        } else if header.name().eq_ignore_ascii_case("content-length") {
            let parsed = parse_content_length_value(header.value()).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidRequest,
                    "request Content-Length is invalid",
                )
            })?;
            if content_length.is_some_and(|existing| existing != parsed) {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "request contains conflicting Content-Length values",
                ));
            }
            content_length = Some(parsed);
        } else if header.name().eq_ignore_ascii_case("transfer-encoding") {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "buffered native requests do not support Transfer-Encoding",
            ));
        } else if header.name().eq_ignore_ascii_case("connection") {
            has_connection = true;
        }
    }
    if host_count > 1 {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "request contains more than one Host header",
        ));
    }
    if content_length.is_some_and(|length| length != request.body().len()) {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "request Content-Length does not match the buffered body",
        ));
    }

    let generated_host = host_count == 0;
    let generated_length = content_length.is_none() && !request.body().is_empty();
    let generated_connection = !has_connection;
    let generated_count = usize::from(generated_host)
        + usize::from(generated_length)
        + usize::from(generated_connection);
    let field_count = request
        .headers()
        .len()
        .checked_add(generated_count)
        .ok_or_else(|| request_header_count_limit(limits.header_count))?;
    if field_count > limits.header_count {
        return Err(request_header_count_limit(limits.header_count));
    }

    let generated_length_value = request.body().len().to_string();
    let mut header_bytes = request.headers().iter().try_fold(0_usize, |total, header| {
        field_wire_len(header.name().as_bytes(), header.value())
            .and_then(|field| total.checked_add(field))
    });
    for (name, value) in [
        generated_host.then_some((b"Host".as_slice(), target.authority.as_bytes())),
        generated_length.then_some((
            b"Content-Length".as_slice(),
            generated_length_value.as_bytes(),
        )),
        generated_connection.then_some((b"Connection".as_slice(), b"close".as_slice())),
    ]
    .into_iter()
    .flatten()
    {
        header_bytes = header_bytes.and_then(|total| {
            field_wire_len(name, value).and_then(|field| total.checked_add(field))
        });
    }
    let Some(header_bytes) = header_bytes else {
        return Err(request_header_bytes_limit(limits.header_bytes));
    };
    if header_bytes > limits.header_bytes {
        return Err(request_header_bytes_limit(limits.header_bytes));
    }

    let method = method_name(request.method());
    let request_line_bytes = method
        .len()
        .checked_add(1)
        .and_then(|bytes| bytes.checked_add(target.path_and_query.len()))
        .and_then(|bytes| bytes.checked_add(b" HTTP/1.1\r\n".len()))
        .ok_or_else(|| request_header_bytes_limit(limits.header_bytes))?;
    let capacity = request_line_bytes
        .checked_add(header_bytes)
        .and_then(|bytes| bytes.checked_add(2))
        .and_then(|bytes| bytes.checked_add(request.body().len()))
        .ok_or_else(|| request_header_bytes_limit(limits.header_bytes))?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(method.as_bytes());
    bytes.push(b' ');
    bytes.extend_from_slice(target.path_and_query.as_bytes());
    bytes.extend_from_slice(b" HTTP/1.1\r\n");
    for header in request.headers() {
        append_header(&mut bytes, header.name().as_bytes(), header.value());
    }
    if generated_host {
        append_header(&mut bytes, b"Host", target.authority.as_bytes());
    }
    if generated_length {
        append_header(
            &mut bytes,
            b"Content-Length",
            generated_length_value.as_bytes(),
        );
    }
    if generated_connection {
        append_header(&mut bytes, b"Connection", b"close");
    }
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(request.body());
    Ok(SerializedRequest {
        bytes,
        response_to_head: matches!(request.method(), Method::Head),
    })
}

struct RequestTarget<'url> {
    scheme: &'url str,
    authority: &'url str,
    path_and_query: String,
}

impl<'url> RequestTarget<'url> {
    fn parse(url: &'url str) -> Result<Self, Error> {
        let Some((scheme, remainder)) = url.split_once("://") else {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "HTTP URL has no scheme separator",
            ));
        };
        let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
        let authority = &remainder[..authority_end];
        let suffix = &remainder[authority_end..];
        let without_fragment = suffix.split_once('#').map_or(suffix, |(value, _)| value);
        let path_and_query = if without_fragment.is_empty() {
            "/".to_owned()
        } else if without_fragment.starts_with('?') {
            format!("/{without_fragment}")
        } else {
            without_fragment.to_owned()
        };
        if path_and_query
            .bytes()
            .any(|byte| !(0x21..=0x7e).contains(&byte))
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "native HTTP currently requires an ASCII request target without raw spaces",
            ));
        }
        Ok(Self {
            scheme,
            authority,
            path_and_query,
        })
    }
}

fn method_name(method: &Method) -> &str {
    match method {
        Method::Get => "GET",
        Method::Head => "HEAD",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
        Method::Other(method) => method,
    }
}

fn field_wire_len(name: &[u8], value: &[u8]) -> Option<usize> {
    name.len()
        .checked_add(value.len())?
        .checked_add(b": \r\n".len())
}

fn append_header(output: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    output.extend_from_slice(name);
    output.extend_from_slice(b": ");
    output.extend_from_slice(value);
    output.extend_from_slice(b"\r\n");
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecodeState {
    Head,
    Fixed { remaining: usize },
    CloseDelimited,
    ChunkSize,
    ChunkData { remaining: usize },
    ChunkEnd { matched: u8 },
    Trailers,
    Complete,
}

pub(super) struct ResponseDecoder {
    limits: HttpLimits,
    response_to_head: bool,
    state: DecodeState,
    scratch: Vec<u8>,
    status: Option<u16>,
    headers: Vec<Header>,
    body: Vec<u8>,
    informational_responses: u8,
    framing_bytes: usize,
}

impl ResponseDecoder {
    pub(super) fn new(response_to_head: bool, limits: HttpLimits) -> Self {
        Self {
            limits,
            response_to_head,
            state: DecodeState::Head,
            scratch: Vec::new(),
            status: None,
            headers: Vec::new(),
            body: Vec::new(),
            informational_responses: 0,
            framing_bytes: 0,
        }
    }

    pub(super) fn ingest(&mut self, bytes: &[u8]) -> Result<Option<Response>, Error> {
        if self.state == DecodeState::Complete {
            return Err(Error::new(
                ErrorKind::Internal,
                "native HTTP decoder received bytes after completion",
            ));
        }
        for byte in bytes {
            if let Some(response) = self.consume(*byte)? {
                return Ok(Some(response));
            }
        }
        Ok(None)
    }

    pub(super) fn eof(&mut self) -> Result<Option<Response>, Error> {
        match self.state {
            DecodeState::CloseDelimited => self.complete().map(Some),
            DecodeState::Complete => Ok(None),
            DecodeState::Head => Err(Error::transport(
                TransportStage::Receive,
                "the peer closed before a complete HTTP response head arrived",
            )),
            DecodeState::Fixed { .. } => Err(Error::transport(
                TransportStage::Receive,
                "the peer closed before the Content-Length body completed",
            )),
            DecodeState::ChunkSize
            | DecodeState::ChunkData { .. }
            | DecodeState::ChunkEnd { .. }
            | DecodeState::Trailers => Err(http_error(
                "the peer closed before the chunked response completed",
            )),
        }
    }

    fn consume(&mut self, byte: u8) -> Result<Option<Response>, Error> {
        match self.state {
            DecodeState::Head => {
                self.push_scratch(byte, "response head", false)?;
                if self.scratch.ends_with(b"\r\n\r\n") {
                    let parsed =
                        parse_response_head(&self.scratch, self.response_to_head, self.limits)?;
                    self.scratch.clear();
                    match parsed {
                        ParsedHead::Informational => {
                            self.informational_responses = self
                                .informational_responses
                                .checked_add(1)
                                .ok_or_else(|| http_error("too many informational responses"))?;
                            if self.informational_responses > MAX_INFORMATIONAL_RESPONSES {
                                return Err(http_error("too many informational responses"));
                            }
                        }
                        ParsedHead::Final {
                            status,
                            headers,
                            framing,
                        } => {
                            self.status = Some(status);
                            self.headers = headers;
                            match framing {
                                BodyFraming::None | BodyFraming::Fixed(0) => {
                                    return self.complete().map(Some);
                                }
                                BodyFraming::Fixed(remaining) => {
                                    self.state = DecodeState::Fixed { remaining };
                                }
                                BodyFraming::Chunked => {
                                    self.state = DecodeState::ChunkSize;
                                }
                                BodyFraming::CloseDelimited => {
                                    self.state = DecodeState::CloseDelimited;
                                }
                            }
                        }
                    }
                }
            }
            DecodeState::Fixed { remaining } => {
                self.push_body(byte)?;
                if remaining == 1 {
                    return self.complete().map(Some);
                }
                self.state = DecodeState::Fixed {
                    remaining: remaining - 1,
                };
            }
            DecodeState::CloseDelimited => self.push_body(byte)?,
            DecodeState::ChunkSize => {
                self.push_scratch(byte, "chunk-size line", true)?;
                if self.scratch.ends_with(b"\r\n") {
                    let (used, size) = match httparse::parse_chunk_size(&self.scratch) {
                        Ok(httparse::Status::Complete(parsed)) => parsed,
                        Ok(httparse::Status::Partial) | Err(_) => {
                            return Err(http_error("response contains an invalid chunk size"));
                        }
                    };
                    if used != self.scratch.len() {
                        return Err(http_error("response chunk size has trailing bytes"));
                    }
                    self.scratch.clear();
                    let size = usize::try_from(size)
                        .map_err(|_| response_body_limit(self.limits.body_bytes))?;
                    if size > self.limits.body_bytes.saturating_sub(self.body.len()) {
                        return Err(response_body_limit(self.limits.body_bytes));
                    }
                    self.state = if size == 0 {
                        DecodeState::Trailers
                    } else {
                        DecodeState::ChunkData { remaining: size }
                    };
                }
            }
            DecodeState::ChunkData { remaining } => {
                self.push_body(byte)?;
                self.state = if remaining == 1 {
                    DecodeState::ChunkEnd { matched: 0 }
                } else {
                    DecodeState::ChunkData {
                        remaining: remaining - 1,
                    }
                };
            }
            DecodeState::ChunkEnd { matched: 0 } if byte == b'\r' => {
                self.count_framing_byte("chunk terminator")?;
                self.state = DecodeState::ChunkEnd { matched: 1 };
            }
            DecodeState::ChunkEnd { matched: 1 } if byte == b'\n' => {
                self.count_framing_byte("chunk terminator")?;
                self.state = DecodeState::ChunkSize;
            }
            DecodeState::ChunkEnd { .. } => {
                return Err(http_error("response chunk data is not followed by CRLF"));
            }
            DecodeState::Trailers => {
                self.push_scratch(byte, "response trailers", true)?;
                if self.scratch == b"\r\n" || self.scratch.ends_with(b"\r\n\r\n") {
                    validate_trailers(&self.scratch, self.limits)?;
                    self.scratch.clear();
                    return self.complete().map(Some);
                }
            }
            DecodeState::Complete => {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "native HTTP decoder advanced after completion",
                ));
            }
        }
        Ok(None)
    }

    fn push_scratch(&mut self, byte: u8, context: &str, framing: bool) -> Result<(), Error> {
        if self.scratch.len() >= self.limits.header_bytes {
            return Err(Error::limit(
                LimitKind::ResponseHeaderBytes,
                format!(
                    "{context} exceeds the configured {} byte limit",
                    self.limits.header_bytes
                ),
            ));
        }
        if framing {
            self.count_framing_byte(context)?;
        }
        self.scratch.push(byte);
        Ok(())
    }

    fn count_framing_byte(&mut self, context: &str) -> Result<(), Error> {
        if self.framing_bytes >= self.limits.header_bytes {
            return Err(Error::limit(
                LimitKind::ResponseHeaderBytes,
                format!(
                    "{context} exceeds the configured {} byte framing-metadata limit",
                    self.limits.header_bytes
                ),
            ));
        }
        self.framing_bytes += 1;
        Ok(())
    }

    fn push_body(&mut self, byte: u8) -> Result<(), Error> {
        if self.body.len() >= self.limits.body_bytes {
            return Err(response_body_limit(self.limits.body_bytes));
        }
        self.body.push(byte);
        Ok(())
    }

    fn complete(&mut self) -> Result<Response, Error> {
        let status = self.status.take().ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                "native HTTP decoder completed without a final status",
            )
        })?;
        self.state = DecodeState::Complete;
        Ok(Response::new(
            status,
            std::mem::take(&mut self.headers),
            std::mem::take(&mut self.body),
        ))
    }
}

enum ParsedHead {
    Informational,
    Final {
        status: u16,
        headers: Vec<Header>,
        framing: BodyFraming,
    },
}

#[derive(Clone, Copy)]
enum BodyFraming {
    None,
    Fixed(usize),
    Chunked,
    CloseDelimited,
}

fn parse_response_head(
    bytes: &[u8],
    response_to_head: bool,
    limits: HttpLimits,
) -> Result<ParsedHead, Error> {
    let mut slots = vec![httparse::EMPTY_HEADER; limits.header_count];
    let mut parsed = httparse::Response::new(&mut slots);
    let consumed = match parsed.parse(bytes) {
        Ok(httparse::Status::Complete(consumed)) => consumed,
        Ok(httparse::Status::Partial) => {
            return Err(http_error("HTTP response head ended prematurely"));
        }
        Err(httparse::Error::TooManyHeaders) => {
            return Err(response_header_count_limit(limits.header_count));
        }
        Err(error) => {
            return Err(http_error(format!(
                "HTTP response head is malformed: {error}"
            )));
        }
    };
    if consumed != bytes.len() {
        return Err(http_error("HTTP response head has trailing bytes"));
    }
    let version = parsed
        .version
        .ok_or_else(|| http_error("HTTP response has no version"))?;
    if !matches!(version, 0 | 1) {
        return Err(http_error("HTTP response version is unsupported"));
    }
    let status = parsed
        .code
        .filter(|status| (100..=599).contains(status))
        .ok_or_else(|| http_error("HTTP response status is invalid"))?;
    if (100..200).contains(&status) {
        if status == 101 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "HTTP protocol switching is not supported",
            ));
        }
        return Ok(ParsedHead::Informational);
    }

    let headers = parsed
        .headers
        .iter()
        .map(|header| Header::new(header.name, header.value.to_vec()))
        .collect::<Vec<_>>();
    let content_length = response_content_length(&headers)?;
    let transfer_encoding = response_transfer_encoding(&headers)?;
    if transfer_encoding && content_length.is_some() {
        return Err(http_error(
            "HTTP response contains both Transfer-Encoding and Content-Length",
        ));
    }
    let framing = if response_to_head || matches!(status, 204 | 205 | 304) {
        BodyFraming::None
    } else if transfer_encoding {
        BodyFraming::Chunked
    } else if let Some(length) = content_length {
        if length > limits.body_bytes {
            return Err(response_body_limit(limits.body_bytes));
        }
        BodyFraming::Fixed(length)
    } else {
        BodyFraming::CloseDelimited
    };
    Ok(ParsedHead::Final {
        status,
        headers,
        framing,
    })
}

fn response_content_length(headers: &[Header]) -> Result<Option<usize>, Error> {
    let mut length = None;
    for header in headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("content-length"))
    {
        let parsed = parse_content_length_value(header.value())?;
        if length.is_some_and(|existing| existing != parsed) {
            return Err(http_error(
                "HTTP response contains conflicting Content-Length values",
            ));
        }
        length = Some(parsed);
    }
    Ok(length)
}

fn parse_content_length_value(value: &[u8]) -> Result<usize, Error> {
    let mut parsed = None;
    for item in value.split(|byte| *byte == b',') {
        let item = trim_ows(item);
        if item.is_empty() || !item.iter().all(u8::is_ascii_digit) {
            return Err(http_error("HTTP Content-Length is invalid"));
        }
        let text =
            std::str::from_utf8(item).map_err(|_| http_error("HTTP Content-Length is invalid"))?;
        let value = text
            .parse::<usize>()
            .map_err(|_| http_error("HTTP Content-Length is too large"))?;
        if parsed.is_some_and(|existing| existing != value) {
            return Err(http_error(
                "HTTP message contains conflicting Content-Length values",
            ));
        }
        parsed = Some(value);
    }
    parsed.ok_or_else(|| http_error("HTTP Content-Length is empty"))
}

fn response_transfer_encoding(headers: &[Header]) -> Result<bool, Error> {
    let mut codings = Vec::new();
    for header in headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("transfer-encoding"))
    {
        for coding in header.value().split(|byte| *byte == b',') {
            let coding = trim_ows(coding);
            if coding.is_empty() {
                return Err(http_error("HTTP Transfer-Encoding is invalid"));
            }
            codings.push(coding);
        }
    }
    if codings.is_empty() {
        return Ok(false);
    }
    if codings.len() != 1 || !codings[0].eq_ignore_ascii_case(b"chunked") {
        return Err(http_error(
            "native HTTP supports only a single chunked transfer coding",
        ));
    }
    Ok(true)
}

fn validate_trailers(bytes: &[u8], limits: HttpLimits) -> Result<(), Error> {
    let mut slots = vec![httparse::EMPTY_HEADER; limits.header_count];
    let (consumed, headers) = match httparse::parse_headers(bytes, &mut slots) {
        Ok(httparse::Status::Complete(parsed)) => parsed,
        Ok(httparse::Status::Partial) => {
            return Err(http_error("HTTP response trailers ended prematurely"));
        }
        Err(httparse::Error::TooManyHeaders) => {
            return Err(response_header_count_limit(limits.header_count));
        }
        Err(error) => {
            return Err(http_error(format!(
                "HTTP response trailers are malformed: {error}"
            )));
        }
    };
    if consumed != bytes.len() {
        return Err(http_error("HTTP response trailers have trailing bytes"));
    }
    if headers.iter().any(|header| {
        matches!(
            header.name.to_ascii_lowercase().as_str(),
            "content-length" | "transfer-encoding" | "host"
        )
    }) {
        return Err(http_error(
            "HTTP response trailers contain a forbidden framing field",
        ));
    }
    Ok(())
}

fn trim_ows(mut value: &[u8]) -> &[u8] {
    while matches!(value.first(), Some(b' ' | b'\t')) {
        value = &value[1..];
    }
    while matches!(value.last(), Some(b' ' | b'\t')) {
        value = &value[..value.len() - 1];
    }
    value
}

fn http_error(message: impl Into<String>) -> Error {
    Error::transport(TransportStage::Http, message)
}

fn request_header_bytes_limit(limit: usize) -> Error {
    Error::limit(
        LimitKind::RequestHeaderBytes,
        format!("request headers exceed the configured {limit} byte limit"),
    )
}

fn request_header_count_limit(limit: usize) -> Error {
    Error::limit(
        LimitKind::RequestHeaderCount,
        format!("request headers exceed the configured {limit} field limit"),
    )
}

fn response_header_count_limit(limit: usize) -> Error {
    Error::limit(
        LimitKind::ResponseHeaderCount,
        format!("response headers exceed the configured {limit} field limit"),
    )
}

fn response_body_limit(limit: usize) -> Error {
    Error::limit(
        LimitKind::ResponseBodyBytes,
        format!("response body exceeds the configured {limit} byte limit"),
    )
}

/// Private cleartext factory used to prove HTTP framing over the accepted reactor. Ordinary
/// `Engine::new` does not select it while DNS, TLS, redirects, and the remaining parity gates are
/// incomplete.
#[allow(dead_code)]
pub(super) struct NativeHttpFactory {
    limits: HttpLimits,
    resolver: Option<ResolverConfig>,
}

#[allow(dead_code)]
impl NativeHttpFactory {
    pub(super) fn new(config: &EngineConfig) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            resolver: None,
        }
    }

    pub(super) fn new_with_nameserver(config: &EngineConfig, nameserver: SocketAddr) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
        }
    }
}

impl BackendFactory for NativeHttpFactory {
    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error> {
        let backend = NativeHttpBackend::new(self.limits, self.resolver)?;
        let waker = backend.reactor.waker();
        shared.queue.set_external_waker(Some(Arc::new(move || {
            waker.wake().map_err(|error| {
                Error::new(
                    ErrorKind::Internal,
                    format!("native HTTP command wake failed: {error}"),
                )
            })
        })));
        Ok(Box::new(backend))
    }
}

struct HttpTransfer {
    request_id: RequestId,
    decoder: ResponseDecoder,
    body_bearing: bool,
    response_started: bool,
    connected: bool,
    connect_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    inactivity_timeout: Option<Duration>,
    inactivity_deadline: Option<Instant>,
}

impl HttpTransfer {
    fn next_deadline(&self) -> Option<Instant> {
        [
            self.total_deadline,
            (!self.connected).then_some(self.connect_deadline).flatten(),
            self.inactivity_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn note_progress(&mut self, now: Instant, connected: bool) -> Option<Instant> {
        self.connected |= connected;
        self.inactivity_deadline = self
            .inactivity_timeout
            .and_then(|timeout| now.checked_add(timeout));
        self.next_deadline()
    }

    fn expired_timeout(&self, now: Instant) -> TimeoutKind {
        if self.total_deadline.is_some_and(|deadline| deadline <= now) {
            TimeoutKind::Total
        } else if !self.connected
            && self
                .connect_deadline
                .is_some_and(|deadline| deadline <= now)
        {
            TimeoutKind::Connect
        } else if self
            .inactivity_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            TimeoutKind::Inactivity
        } else {
            TimeoutKind::Unknown
        }
    }
}

struct NativeHttpBackend {
    reactor: NativeReactor,
    resolver: Option<NativeResolver>,
    limits: HttpLimits,
    request_to_slot: HashMap<RequestId, SlotId>,
    transfers: HashMap<SlotId, HttpTransfer>,
    request_to_resolve: HashMap<RequestId, ResolveKey>,
    resolves: HashMap<ResolveKey, PendingResolve>,
    next_resolve_key: u64,
}

struct PendingResolve {
    request_id: RequestId,
    serialized: SerializedRequest,
    port: u16,
    body_bearing: bool,
    connect_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    inactivity_timeout: Option<Duration>,
    inactivity_deadline: Option<Instant>,
}

impl PendingResolve {
    fn next_deadline(&self) -> Option<Instant> {
        [
            self.connect_deadline,
            self.total_deadline,
            self.inactivity_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn expired_timeout(&self, now: Instant) -> TimeoutKind {
        if self.total_deadline.is_some_and(|deadline| deadline <= now) {
            TimeoutKind::Total
        } else if self
            .connect_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            TimeoutKind::Connect
        } else if self
            .inactivity_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            TimeoutKind::Inactivity
        } else {
            TimeoutKind::Unknown
        }
    }
}

impl NativeHttpBackend {
    fn new(limits: HttpLimits, resolver: Option<ResolverConfig>) -> Result<Self, Error> {
        let reactor = NativeReactor::new(256).map_err(native_internal_error)?;
        let resolver = resolver
            .map(|config| NativeResolver::new(config, reactor.waker()))
            .transpose()?;
        Ok(Self {
            reactor,
            resolver,
            limits,
            request_to_slot: HashMap::new(),
            transfers: HashMap::new(),
            request_to_resolve: HashMap::new(),
            resolves: HashMap::new(),
            next_resolve_key: 1,
        })
    }

    fn finish(
        &mut self,
        slot: SlotId,
        completion: Completion,
        completions: &mut Vec<BackendCompletion>,
    ) {
        self.reactor.cancel(slot);
        if let Some(transfer) = self.transfers.remove(&slot) {
            self.request_to_slot.remove(&transfer.request_id);
            completions.push(BackendCompletion {
                id: transfer.request_id,
                completion,
            });
        }
    }

    fn begin_connection(
        &mut self,
        address: SocketAddr,
        mut pending: PendingResolve,
    ) -> Option<Completion> {
        let deadline = pending.next_deadline();
        let outbound_limit = pending.serialized.bytes.len();
        let slot = match self.reactor.connect(
            address,
            deadline,
            outbound_limit,
            self.limits.reactor_receive_limit(),
        ) {
            Ok(slot) => slot,
            Err(failure) => {
                return Some(Completion::Failed(native_transport_error(failure)));
            }
        };
        if let Err(failure) = self.reactor.queue_write(slot, &pending.serialized.bytes) {
            self.reactor.cancel(slot);
            return Some(Completion::Failed(native_transport_error(failure)));
        }
        let request_id = pending.request_id;
        let now = Instant::now();
        pending.inactivity_deadline = pending
            .inactivity_timeout
            .and_then(|timeout| now.checked_add(timeout));
        self.request_to_slot.insert(request_id, slot);
        self.transfers.insert(
            slot,
            HttpTransfer {
                request_id,
                decoder: ResponseDecoder::new(pending.serialized.response_to_head, self.limits),
                body_bearing: pending.body_bearing,
                response_started: false,
                connected: false,
                connect_deadline: pending.connect_deadline,
                total_deadline: pending.total_deadline,
                inactivity_timeout: pending.inactivity_timeout,
                inactivity_deadline: pending.inactivity_deadline,
            },
        );
        None
    }

    fn process_resolver_results(&mut self) -> Result<Vec<BackendCompletion>, Error> {
        let results = match &self.resolver {
            Some(resolver) => resolver.drain()?,
            None => return Ok(Vec::new()),
        };
        let mut completions = Vec::new();
        for result in results {
            let Some(pending) = self.resolves.remove(&result.key) else {
                continue;
            };
            self.request_to_resolve.remove(&pending.request_id);
            match result.result {
                Ok(answer) => {
                    let Some(ip) = answer.addresses.into_iter().next() else {
                        completions.push(BackendCompletion {
                            id: pending.request_id,
                            completion: Completion::Failed(Error::transport(
                                TransportStage::Dns,
                                "the native resolver returned no usable address",
                            )),
                        });
                        continue;
                    };
                    let id = pending.request_id;
                    if let Some(completion) =
                        self.begin_connection(SocketAddr::new(ip, pending.port), pending)
                    {
                        completions.push(BackendCompletion { id, completion });
                    }
                }
                Err(failure) => completions.push(BackendCompletion {
                    id: pending.request_id,
                    completion: Completion::Failed(Error::transport(
                        TransportStage::Dns,
                        failure.message,
                    )),
                }),
            }
        }
        Ok(completions)
    }

    fn expire_resolves(&mut self) -> Result<Vec<BackendCompletion>, Error> {
        let now = Instant::now();
        let expired = self
            .resolves
            .iter()
            .filter_map(|(key, pending)| {
                pending
                    .next_deadline()
                    .is_some_and(|deadline| deadline <= now)
                    .then_some(*key)
            })
            .collect::<Vec<_>>();
        let mut completions = Vec::new();
        for key in expired {
            let Some(pending) = self.resolves.remove(&key) else {
                continue;
            };
            self.request_to_resolve.remove(&pending.request_id);
            if let Some(resolver) = &self.resolver {
                resolver.cancel(key)?;
            }
            let timeout = pending.expired_timeout(now);
            completions.push(BackendCompletion {
                id: pending.request_id,
                completion: Completion::Failed(Error::timeout(
                    timeout,
                    native_timeout_message(timeout),
                )),
            });
        }
        Ok(completions)
    }

    fn next_resolve_key(&mut self) -> Result<ResolveKey, Error> {
        let key = ResolveKey(self.next_resolve_key);
        self.next_resolve_key = self.next_resolve_key.checked_add(1).ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                "the native resolver request identity space is exhausted",
            )
        })?;
        Ok(key)
    }

    fn fail_native(
        &mut self,
        slot: SlotId,
        failure: NativeFailure,
        completions: &mut Vec<BackendCompletion>,
    ) {
        let error = if failure.kind == NativeFailureKind::Read
            && self
                .transfers
                .get(&slot)
                .is_some_and(|transfer| transfer.body_bearing && !transfer.response_started)
        {
            Error::transport(
                TransportStage::Send,
                "the connection failed during a body-bearing exchange before any response bytes arrived",
            )
        } else {
            native_transport_error(failure)
        };
        self.finish(slot, Completion::Failed(error), completions);
    }

    fn note_progress(
        &mut self,
        slot: SlotId,
        connected: bool,
        arm_deadline: bool,
    ) -> Result<(), Error> {
        let deadline = self
            .transfers
            .get_mut(&slot)
            .map(|transfer| transfer.note_progress(Instant::now(), connected));
        if arm_deadline {
            if let Some(deadline) = deadline {
                self.reactor
                    .set_deadline(slot, deadline)
                    .map_err(native_internal_error)?;
            }
        }
        Ok(())
    }

    fn process_events(
        &mut self,
        events: Vec<NativeEvent>,
    ) -> Result<Vec<BackendCompletion>, Error> {
        // A readiness pass can make write progress and then observe a read-side reset before it
        // returns to the protocol owner. The reactor has already removed that socket by the time
        // this batch is processed, so progress remains meaningful but must not re-arm its deadline.
        let failed_slots = events
            .iter()
            .filter_map(|event| match event {
                NativeEvent::Failed(slot, _) => Some(*slot),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut completions = Vec::new();
        for event in events {
            match event {
                NativeEvent::Connected(slot) => {
                    self.note_progress(slot, true, !failed_slots.contains(&slot))?
                }
                NativeEvent::WriteProgress(slot) | NativeEvent::WriteDrained(slot) => {
                    self.note_progress(slot, false, !failed_slots.contains(&slot))?;
                }
                NativeEvent::Data(slot, bytes) => {
                    self.note_progress(slot, false, !failed_slots.contains(&slot))?;
                    let decoded = self.transfers.get_mut(&slot).map(|transfer| {
                        transfer.response_started |= !bytes.is_empty();
                        transfer.decoder.ingest(&bytes)
                    });
                    match decoded {
                        Some(Ok(Some(response))) => {
                            self.finish(slot, Completion::Completed(response), &mut completions)
                        }
                        Some(Err(error)) => {
                            self.finish(slot, Completion::Failed(error), &mut completions);
                        }
                        Some(Ok(None)) | None => {}
                    }
                }
                NativeEvent::PeerClosed(slot) => {
                    let decoded = self
                        .transfers
                        .get_mut(&slot)
                        .map(|transfer| transfer.decoder.eof());
                    match decoded {
                        Some(Ok(Some(response))) => {
                            self.finish(slot, Completion::Completed(response), &mut completions)
                        }
                        Some(Ok(None)) => self.finish(
                            slot,
                            Completion::Failed(Error::transport(
                                TransportStage::Receive,
                                "the peer closed without completing a response",
                            )),
                            &mut completions,
                        ),
                        Some(Err(error)) => {
                            self.finish(slot, Completion::Failed(error), &mut completions);
                        }
                        None => {
                            self.reactor.cancel(slot);
                        }
                    }
                }
                NativeEvent::Failed(slot, failure) => {
                    self.fail_native(slot, failure, &mut completions);
                }
                NativeEvent::DeadlineExpired(slot) => {
                    let now = Instant::now();
                    let deadline = self
                        .transfers
                        .get(&slot)
                        .and_then(HttpTransfer::next_deadline);
                    if deadline.is_some_and(|deadline| deadline <= now) {
                        let timeout = self
                            .transfers
                            .get(&slot)
                            .map_or(TimeoutKind::Unknown, |transfer| {
                                transfer.expired_timeout(now)
                            });
                        self.finish(
                            slot,
                            Completion::Failed(Error::timeout(
                                timeout,
                                native_timeout_message(timeout),
                            )),
                            &mut completions,
                        );
                    } else if let Some(deadline) = deadline {
                        self.reactor
                            .set_deadline(slot, Some(deadline))
                            .map_err(native_internal_error)?;
                    }
                }
            }
        }
        Ok(completions)
    }
}

impl Backend for NativeHttpBackend {
    fn submit(
        &mut self,
        id: RequestId,
        request: Request,
        accepted_at: Instant,
    ) -> Option<Completion> {
        let serialized = match serialize_request(&request, self.limits) {
            Ok(serialized) => serialized,
            Err(error) => return Some(Completion::Failed(error)),
        };
        let origin = match http_origin(request.url(), ErrorKind::InvalidRequest) {
            Ok(origin) => origin,
            Err(error) => return Some(Completion::Failed(error)),
        };
        let options = request.options();
        let connect_deadline = options
            .connect_timeout
            .and_then(|timeout| accepted_at.checked_add(timeout));
        let total_deadline = options
            .total_timeout
            .and_then(|timeout| accepted_at.checked_add(timeout));
        let inactivity_deadline = options
            .inactivity_timeout
            .and_then(|timeout| accepted_at.checked_add(timeout));
        let pending = PendingResolve {
            request_id: id,
            serialized,
            port: origin.port,
            body_bearing: !request.body().is_empty(),
            connect_deadline,
            total_deadline,
            inactivity_timeout: options.inactivity_timeout,
            inactivity_deadline,
        };
        if let Ok(ip) = origin.host.parse::<IpAddr>() {
            return self.begin_connection(SocketAddr::new(ip, origin.port), pending);
        }
        let key = match self.next_resolve_key() {
            Ok(key) => key,
            Err(error) => return Some(Completion::Failed(error)),
        };
        let Some(resolver) = &self.resolver else {
            return Some(Completion::Failed(Error::new(
                ErrorKind::Unsupported,
                "the native HTTP proving Engine requires an injected resolver for hostnames",
            )));
        };
        if let Err(error) = resolver.resolve(key, origin.host) {
            return Some(Completion::Failed(error));
        }
        self.request_to_resolve.insert(id, key);
        self.resolves.insert(key, pending);
        None
    }

    fn cancel(&mut self, id: RequestId) {
        if let Some(key) = self.request_to_resolve.remove(&id) {
            self.resolves.remove(&key);
            if let Some(resolver) = &self.resolver {
                let _cancel_result = resolver.cancel(key);
            }
        }
        if let Some(slot) = self.request_to_slot.remove(&id) {
            self.transfers.remove(&slot);
            self.reactor.cancel(slot);
        }
    }

    fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
        let mut completions = self.process_resolver_results()?;
        completions.extend(self.expire_resolves()?);
        let poll_deadline = if completions.is_empty() {
            deadline
        } else {
            Instant::now()
        };
        let events = self
            .reactor
            .poll(poll_deadline)
            .map_err(native_internal_error)?;
        completions.extend(self.process_events(events)?);
        completions.extend(self.process_resolver_results()?);
        completions.extend(self.expire_resolves()?);
        Ok(completions)
    }

    fn shutdown(&mut self) -> Result<(), ShutdownError> {
        self.request_to_slot.clear();
        self.transfers.clear();
        self.request_to_resolve.clear();
        self.resolves.clear();
        if let Some(resolver) = &mut self.resolver {
            resolver.shutdown().map_err(ShutdownError::new)?;
        }
        self.reactor.shutdown();
        Ok(())
    }

    fn poll_mode(&self) -> PollMode {
        PollMode::Interruptible {
            max_wait: NATIVE_SAFETY_POLL,
        }
    }
}

fn native_internal_error(failure: NativeFailure) -> Error {
    Error::new(ErrorKind::Internal, failure.message)
}

fn native_timeout_message(kind: TimeoutKind) -> &'static str {
    match kind {
        TimeoutKind::Connect => "the native HTTP connection-establishment timeout expired",
        TimeoutKind::Inactivity => "the native HTTP inactivity timeout expired",
        TimeoutKind::Total => "the native HTTP total request timeout expired",
        TimeoutKind::Unknown => "a native HTTP deadline expired without a matching timeout stage",
    }
}

fn native_transport_error(failure: NativeFailure) -> Error {
    match failure.kind {
        NativeFailureKind::Connect => Error::transport(TransportStage::Connect, failure.message),
        NativeFailureKind::Read => Error::transport(TransportStage::Receive, failure.message),
        NativeFailureKind::Write => Error::transport(TransportStage::Send, failure.message),
        NativeFailureKind::OutboundQueueFull => Error::limit(
            LimitKind::RequestBodyBytes,
            "the serialized native request exceeded its bounded send queue",
        ),
        NativeFailureKind::ReceiveLimit => Error::limit(
            LimitKind::ResponseBodyBytes,
            "the native response exceeded its bounded wire allowance",
        ),
        NativeFailureKind::Internal => Error::new(ErrorKind::Internal, failure.message),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::{
        Completion, Engine, EngineConfig, ErrorKind, ExecuteError, LimitKind, Request, TimeoutKind,
        TransportStage,
    };

    const LIMITS: HttpLimits = HttpLimits {
        body_bytes: 1024,
        header_bytes: 1024,
        header_count: 16,
    };

    #[test]
    fn terminal_socket_failure_dominates_same_batch_write_progress() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("batch fixture must bind");
        let address = listener.local_addr().expect("batch fixture address");
        let mut backend =
            NativeHttpBackend::new(LIMITS, None).expect("native backend must construct");
        let deadline = Instant::now() + Duration::from_secs(5);
        let slot = backend
            .reactor
            .connect(address, Some(deadline), 1024, 1024)
            .expect("batch fixture must connect");
        let (_peer, _) = listener.accept().expect("batch fixture must accept");
        let request_id = RequestId {
            engine: 1,
            sequence: 1,
        };
        backend.request_to_slot.insert(request_id, slot);
        backend.transfers.insert(
            slot,
            HttpTransfer {
                request_id,
                decoder: ResponseDecoder::new(false, LIMITS),
                body_bearing: true,
                response_started: false,
                connected: true,
                connect_deadline: None,
                total_deadline: Some(deadline),
                inactivity_timeout: Some(Duration::from_secs(1)),
                inactivity_deadline: Some(deadline),
            },
        );

        assert!(backend.reactor.cancel(slot));
        let completions = backend
            .process_events(vec![
                NativeEvent::WriteProgress(slot),
                NativeEvent::Failed(
                    slot,
                    NativeFailure {
                        kind: NativeFailureKind::Read,
                        message: "simulated same-batch reset".to_owned(),
                    },
                ),
            ])
            .expect("same-batch progress must not re-arm a removed socket");

        assert_eq!(completions.len(), 1);
        let Completion::Failed(error) = &completions[0].completion else {
            panic!("same-batch reset must fail the request");
        };
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert_eq!(error.transport_stage(), Some(TransportStage::Send));
    }

    fn assert_socket_closed(stream: &mut std::net::TcpStream, buffer: &mut [u8], context: &str) {
        match stream.read(buffer) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) => {}
            Ok(read) => panic!("{context}: received {read} bytes instead of socket close"),
            Err(error) => panic!("{context}: did not observe socket close: {error}"),
        }
    }

    fn decode_fragmented(
        response_to_head: bool,
        bytes: &[u8],
        eof: bool,
    ) -> Result<Response, Error> {
        let mut decoder = ResponseDecoder::new(response_to_head, LIMITS);
        for byte in bytes {
            if let Some(response) = decoder.ingest(std::slice::from_ref(byte))? {
                return Ok(response);
            }
        }
        if eof {
            if let Some(response) = decoder.eof()? {
                return Ok(response);
            }
        }
        Err(Error::new(
            ErrorKind::Internal,
            "test response did not reach terminal framing",
        ))
    }

    fn decode_at_split(bytes: &[u8], split: usize, eof: bool) -> Result<Response, Error> {
        let mut decoder = ResponseDecoder::new(false, LIMITS);
        for part in [&bytes[..split], &bytes[split..]] {
            if let Some(response) = decoder.ingest(part)? {
                return Ok(response);
            }
        }
        if eof {
            if let Some(response) = decoder.eof()? {
                return Ok(response);
            }
        }
        Err(Error::new(
            ErrorKind::Internal,
            "split test response did not reach terminal framing",
        ))
    }

    #[test]
    fn serializes_origin_form_host_lengths_and_binary_headers() {
        let request = Request::post("http://example.test:8080/path?q=yes#ignored")
            .header("X-Binary", vec![0x80, b'x'])
            .body(b"hello".to_vec())
            .build()
            .expect("request must build");
        let serialized = serialize_request(&request, LIMITS).expect("request must serialize");
        assert_eq!(
            serialized.bytes,
            b"POST /path?q=yes HTTP/1.1\r\nX-Binary: \x80x\r\nHost: example.test:8080\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
        );
        assert!(!serialized.response_to_head);

        let query_only = Request::get("http://example.test?x=1")
            .build()
            .expect("query request must build");
        let serialized = serialize_request(&query_only, LIMITS).expect("request must serialize");
        assert!(serialized.bytes.starts_with(b"GET /?x=1 HTTP/1.1\r\n"));
    }

    #[test]
    fn request_serialization_rejects_ambiguous_framing_and_generated_limit_overflow() {
        for request in [
            Request::post("http://example.test/")
                .header("Content-Length", "2")
                .body(b"one".to_vec())
                .build()
                .expect("portable request must build"),
            Request::post("http://example.test/")
                .header("Transfer-Encoding", "chunked")
                .body(b"one".to_vec())
                .build()
                .expect("portable request must build"),
        ] {
            assert!(serialize_request(&request, LIMITS).is_err());
        }

        let request = Request::get("http://example.test/")
            .build()
            .expect("request must build");
        let error = serialize_request(
            &request,
            HttpLimits {
                header_count: 1,
                ..LIMITS
            },
        )
        .expect_err("generated Host and Connection must count");
        assert_eq!(error.limit_kind(), Some(LimitKind::RequestHeaderCount));
    }

    #[test]
    fn fragmented_informational_and_fixed_length_response_completes() {
        let response = decode_fragmented(
            false,
            b"HTTP/1.1 103 Early Hints\r\nLink: </x>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Test: yes\r\n\r\nhello",
            false,
        )
        .expect("fragmented fixed response must complete");
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"hello");
        assert!(response.headers().iter().any(|header| {
            header.name().eq_ignore_ascii_case("x-test") && header.value() == b"yes"
        }));
    }

    #[test]
    fn every_two_part_fragmentation_preserves_valid_and_invalid_results() {
        let valid = [
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".as_slice(),
                false,
                b"hello".as_slice(),
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhe\r\n3;ext=yes\r\nllo\r\n0\r\nX-T: y\r\n\r\n"
                    .as_slice(),
                false,
                b"hello".as_slice(),
            ),
            (
                b"HTTP/1.0 200 OK\r\n\r\nhello".as_slice(),
                true,
                b"hello".as_slice(),
            ),
        ];
        for (bytes, eof, expected) in valid {
            for split in 0..=bytes.len() {
                let response = decode_at_split(bytes, split, eof)
                    .unwrap_or_else(|error| panic!("split {split} failed: {error}"));
                assert_eq!(response.body(), expected, "split {split}");
            }
        }

        let malformed = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nxx".as_slice(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nx".as_slice(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nxX".as_slice(),
        ];
        for bytes in malformed {
            for split in 0..=bytes.len() {
                let error = decode_at_split(bytes, split, true)
                    .expect_err("malformed response must fail at every split");
                assert_eq!(
                    error.transport_stage(),
                    Some(TransportStage::Http),
                    "split {split}: {error}"
                );
            }
        }
    }

    #[test]
    fn chunk_extensions_and_trailers_are_framed_but_trailers_are_not_exposed() {
        let response = decode_fragmented(
            false,
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;lab=yes\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: yes\r\n\r\n",
            false,
        )
        .expect("chunked response must complete");
        assert_eq!(response.body(), b"hello world");
        assert_eq!(response.headers().len(), 1);
    }

    #[test]
    fn no_body_and_close_delimited_responses_use_explicit_completion_rules() {
        let head = decode_fragmented(
            true,
            b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\n",
            false,
        )
        .expect("HEAD response must complete at the head");
        assert!(head.body().is_empty());

        let no_content = decode_fragmented(false, b"HTTP/1.1 204 No Content\r\n\r\n", false)
            .expect("204 response must complete at the head");
        assert!(no_content.body().is_empty());

        let close = decode_fragmented(
            false,
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nbody",
            true,
        )
        .expect("close-delimited response must complete at EOF");
        assert_eq!(close.body(), b"body");
    }

    #[test]
    fn malformed_and_premature_messages_have_portable_stage_mappings() {
        for bytes in [
            &b"NOT HTTP\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 1\r\n\r\n1\r\nx\r\n0\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n"[..],
        ] {
            let error = decode_fragmented(false, bytes, true)
                .expect_err("malformed message must fail");
            assert_eq!(error.transport_stage(), Some(TransportStage::Http));
        }

        let fixed = decode_fragmented(
            false,
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhi",
            true,
        )
        .expect_err("short fixed response must fail");
        assert_eq!(fixed.transport_stage(), Some(TransportStage::Receive));

        let chunked = decode_fragmented(
            false,
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhi",
            true,
        )
        .expect_err("short chunked response must fail");
        assert_eq!(chunked.transport_stage(), Some(TransportStage::Http));
    }

    #[test]
    fn response_limits_fail_before_owned_buffers_cross_their_bounds() {
        let limits = HttpLimits {
            body_bytes: 3,
            header_bytes: 64,
            header_count: 2,
        };
        let mut decoder = ResponseDecoder::new(false, limits);
        let error = decoder
            .ingest(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n")
            .expect_err("declared oversize body must fail at the head");
        assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
        assert!(decoder.body.is_empty());

        let mut decoder = ResponseDecoder::new(false, limits);
        let error = decoder
            .ingest(b"HTTP/1.1 200 OK\r\nX-Long: 1234567890123456789012345678901234567890")
            .expect_err("oversize head must fail while receiving it");
        assert_eq!(error.limit_kind(), Some(LimitKind::ResponseHeaderBytes));
        assert_eq!(decoder.scratch.len(), limits.header_bytes);
    }

    #[test]
    fn native_engine_composes_serialization_fragmentation_and_canonical_completion() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let address = listener.local_addr().expect("HTTP fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 256];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("request head must read");
                assert_ne!(read, 0, "client closed before request head");
                received.extend_from_slice(&buffer[..read]);
            }
            let head_end = received
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("request head delimiter")
                + 4;
            while received.len() < head_end + 4 {
                let read = stream.read(&mut buffer).expect("request body must read");
                assert_ne!(read, 0, "client closed before request body");
                received.extend_from_slice(&buffer[..read]);
            }
            assert!(received.starts_with(b"POST /submit?q=1 HTTP/1.1\r\n"));
            assert!(
                received[..head_end]
                    .windows(b"Host: ".len())
                    .any(|window| window == b"Host: ")
            );
            assert_eq!(&received[head_end..head_end + 4], b"ping");

            let response = b"HTTP/1.1 100 Continue\r\nX-Ignored: yes\r\n\r\nHTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\nX-Final: yes\r\n\r\n4\r\npong\r\n0\r\nX-Trailer: yes\r\n\r\n";
            for byte in response {
                stream
                    .write_all(std::slice::from_ref(byte))
                    .expect("fragmented response byte must write");
                thread::yield_now();
            }
        });

        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("native HTTP Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::post(format!("http://{address}/submit?q=1#ignored"))
                    .body(b"ping".to_vec())
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("native HTTP request must build"),
            )
            .expect("native HTTP request must complete");
        assert_eq!(response.status(), 201);
        assert_eq!(response.body(), b"pong");
        assert!(response.headers().iter().any(|header| {
            header.name().eq_ignore_ascii_case("x-final") && header.value() == b"yes"
        }));
        assert!(
            !response
                .headers()
                .iter()
                .any(|header| header.name().eq_ignore_ascii_case("x-trailer"))
        );
        engine.shutdown().expect("native HTTP Engine must stop");
        server.join().expect("HTTP fixture must join");
    }

    #[test]
    fn manual_native_http_engine_uses_the_same_canonical_completion() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let address = listener.local_addr().expect("HTTP fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 256];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("request head must read");
                assert_ne!(read, 0, "client closed before request head");
                received.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nmanual")
                .expect("manual response must write");
        });

        let config = EngineConfig::manual();
        let backend = NativeHttpBackend::new(HttpLimits::from_config(&config), None)
            .expect("manual native HTTP backend must construct");
        let mut engine = Engine::with_backend(config, Box::new(backend))
            .expect("manual native HTTP Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get(format!("http://{address}/manual"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("manual request must build"),
            )
            .expect("manual request must submit");
        let completion = engine
            .drive_until(pending)
            .expect("manual Engine must drive the HTTP request");
        let Completion::Completed(response) = completion else {
            panic!("manual native HTTP request did not complete");
        };
        assert_eq!(response.body(), b"manual");
        engine
            .shutdown()
            .expect("manual native HTTP Engine must stop");
        server.join().expect("HTTP fixture must join");
    }

    #[test]
    fn native_engine_completes_close_delimited_body_on_peer_fin() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let address = listener.local_addr().expect("HTTP fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 256];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("request head must read");
                assert_ne!(read, 0, "client closed before request head");
                received.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nclose-body")
                .expect("close-delimited response must write");
            stream
                .shutdown(std::net::Shutdown::Write)
                .expect("response write half must close");
            assert_socket_closed(&mut stream, &mut buffer, "close-delimited completion");
        });

        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("native HTTP Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/close"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("close-delimited request must build"),
            )
            .expect("close-delimited response must complete");
        assert_eq!(response.body(), b"close-body");
        engine.shutdown().expect("native HTTP Engine must stop");
        server.join().expect("HTTP fixture must join");
    }

    #[test]
    fn native_engine_cancellation_closes_a_stalled_http_socket_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let address = listener.local_addr().expect("HTTP fixture address");
        let (head_tx, head_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("fixture timeout must configure");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 256];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("request head must read");
                assert_ne!(read, 0, "client closed before request head");
                received.extend_from_slice(&buffer[..read]);
            }
            head_tx.send(()).expect("head observation must send");
            assert_socket_closed(&mut stream, &mut buffer, "stalled request cancellation");
        });

        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("native HTTP Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get(format!("http://{address}/stall"))
                    .total_timeout(Duration::from_secs(5))
                    .build()
                    .expect("native HTTP request must build"),
            )
            .expect("native HTTP request must submit");
        head_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server must observe request head");
        let started = Instant::now();
        pending
            .handle()
            .cancel()
            .expect("request cancellation must win");
        assert!(matches!(pending.wait(), Completion::Cancelled));
        assert!(started.elapsed() < Duration::from_millis(500));
        engine.shutdown().expect("native HTTP Engine must stop");
        server.join().expect("HTTP fixture must join");
    }

    #[test]
    fn native_http_stalled_response_classifies_inactivity_and_total_timeouts() {
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("native HTTP Engine must construct");
        let client = engine.client();

        for timeout_kind in [TimeoutKind::Inactivity, TimeoutKind::Total] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
            let address = listener.local_addr().expect("HTTP fixture address");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("fixture timeout must configure");
                let mut received = Vec::new();
                let mut buffer = [0_u8; 256];
                while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).expect("request head must read");
                    assert_ne!(read, 0, "client closed before request head");
                    received.extend_from_slice(&buffer[..read]);
                }
                assert_socket_closed(&mut stream, &mut buffer, "HTTP timeout");
            });

            let builder = Request::get(format!("http://{address}/timeout"));
            let request = match timeout_kind {
                TimeoutKind::Inactivity => builder
                    .inactivity_timeout(Duration::from_millis(40))
                    .total_timeout(Duration::from_secs(2)),
                TimeoutKind::Total => builder.total_timeout(Duration::from_millis(40)),
                _ => panic!("unexpected timeout fixture kind"),
            }
            .build()
            .expect("timeout request must build");
            let started = Instant::now();
            let error = client
                .execute(request)
                .expect_err("stalled native HTTP response must time out");
            let ExecuteError::Failed(error) = error else {
                panic!("timeout request returned the wrong terminal category");
            };
            assert_eq!(error.kind(), ErrorKind::Timeout);
            assert_eq!(error.timeout_kind(), Some(timeout_kind));
            server.join().expect("timeout fixture must join");
            assert!(started.elapsed() < Duration::from_millis(500));
        }
        engine.shutdown().expect("native HTTP Engine must stop");
    }

    #[test]
    fn useful_response_progress_refreshes_the_inactivity_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
        let address = listener.local_addr().expect("HTTP fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 256];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("request head must read");
                assert_ne!(read, 0, "client closed before request head");
                received.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n")
                .expect("response head must write");
            for byte in b"progress" {
                thread::sleep(Duration::from_millis(50));
                stream
                    .write_all(std::slice::from_ref(byte))
                    .expect("progress byte must write");
                stream.flush().expect("progress byte must flush");
            }
        });

        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("native HTTP Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/progress"))
                    .inactivity_timeout(Duration::from_millis(200))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("progress request must build"),
            )
            .expect("useful progress must keep the request alive");
        assert_eq!(response.body(), b"progress");
        engine.shutdown().expect("native HTTP Engine must stop");
        server.join().expect("HTTP fixture must join");
    }

    #[test]
    fn cancellation_closes_every_http_parse_boundary() {
        let boundaries = [
            ("before response", b"".as_slice()),
            ("status line", b"HTTP/1.".as_slice()),
            ("header value", b"HTTP/1.1 200 OK\r\nX-Test: par".as_slice()),
            (
                "before fixed body",
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n".as_slice(),
            ),
            (
                "inside fixed body",
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhe".as_slice(),
            ),
            (
                "chunk size",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nA".as_slice(),
            ),
            (
                "chunk data",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhe".as_slice(),
            ),
            (
                "chunk terminator",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r".as_slice(),
            ),
            (
                "trailers",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n0\r\nX-Trailer: par"
                    .as_slice(),
            ),
        ];
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("native HTTP Engine must construct");
        let client = engine.client();

        for (index, (name, prefix)) in boundaries.into_iter().enumerate() {
            let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture must bind");
            let address = listener.local_addr().expect("HTTP fixture address");
            let (stalled_tx, stalled_rx) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("HTTP fixture must accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("fixture timeout must configure");
                let mut received = Vec::new();
                let mut buffer = [0_u8; 256];
                while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).expect("request head must read");
                    assert_ne!(read, 0, "client closed before request head");
                    received.extend_from_slice(&buffer[..read]);
                }
                stream.write_all(prefix).expect("stalled prefix must write");
                stream.flush().expect("stalled prefix must flush");
                stalled_tx.send(()).expect("stall observation must send");
                assert_socket_closed(&mut stream, &mut buffer, name);
            });

            let pending = client
                .submit(
                    Request::get(format!("http://{address}/case-{index}"))
                        .total_timeout(Duration::from_secs(5))
                        .build()
                        .expect("boundary request must build"),
                )
                .expect("boundary request must submit");
            stalled_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("server must reach parse boundary");
            let started = Instant::now();
            pending.handle().cancel().expect("boundary cancel must win");
            assert!(matches!(pending.wait(), Completion::Cancelled), "{name}");
            server.join().expect("boundary fixture must join");
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "{name}: network cancellation took {:?}",
                started.elapsed()
            );
        }
        engine.shutdown().expect("native HTTP Engine must stop");
    }
}
