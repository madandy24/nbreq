//! Incremental HTTP/1.1 serialization and response framing for the native backend.
//!
//! `httparse` recognizes response heads and chunk-size lines. NBReq owns request policy, body
//! framing, limits, EOF semantics, and the state transition that eventually releases a socket.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::native::{
    NATIVE_SAFETY_POLL, NativeEvent, NativeFailure, NativeFailureKind, NativeReactor, SlotId,
};
use super::native_dns::{
    NativeResolver, PublicLookupOutcome, PublicResolveSpec, ResolveKey, ResolverConfig,
};
use super::native_tls::{
    NativeTls, NativeTlsConfigs, TlsProgress, TlsStreamProgress, encrypted_outbound_limit,
    encrypted_receive_limit,
};
use super::{Backend, BackendCompletion, BackendFactory, BackendResolveCompletion, PollMode};
use crate::metrics::Metrics;
use crate::registry::{Shared, TcpConnectSink};
use crate::stream::{ResponsePushError, ResponseSink, UploadBody, UploadFraming, UploadPoll};
use crate::tcp::io::TcpIoOwner;
use crate::types::{http_origin, redirected_request};
use crate::{
    AddressFamily, AddressOrder, CacheMode, Completion, EngineConfig, Error, ErrorKind, Header,
    LimitKind, Method, Request, RequestId, ResolveCompletion, ResolveRequest, ResolveResponse,
    ResolveStatus, ResolvedAddress, Response, ResponseHead, ShutdownError, StreamRequest,
    TcpConnectRequest, TcpConnectTarget, TimeoutKind, TlsFailure, TransportStage,
};

const MAX_INFORMATIONAL_RESPONSES: u8 = 8;
const STACK_RESPONSE_HEADER_SLOTS: usize = 32;
#[derive(Clone, Copy)]
struct ConnectionLimits {
    global: usize,
    per_origin: usize,
    idle_global: usize,
    idle_per_origin: usize,
    idle_timeout: Duration,
}

impl ConnectionLimits {
    fn from_config(config: &EngineConfig) -> Self {
        Self {
            global: config.max_connections().get(),
            per_origin: config.max_connections_per_origin().get(),
            idle_global: config.max_idle_connections(),
            idle_per_origin: config.max_idle_connections_per_origin(),
            idle_timeout: config.idle_connection_timeout(),
        }
    }
}

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
    permits_reuse: bool,
}

#[cfg(test)]
pub(super) fn serialize_request(
    request: &Request,
    limits: HttpLimits,
) -> Result<SerializedRequest, Error> {
    serialize_request_with_upload(request, limits, None)
}

fn serialize_request_with_upload(
    request: &Request,
    limits: HttpLimits,
    upload: Option<UploadFraming>,
) -> Result<SerializedRequest, Error> {
    request.validate()?;
    let target = RequestTarget::parse(request.url())?;
    if !matches!(
        target.scheme.to_ascii_lowercase().as_str(),
        "http" | "https"
    ) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "the native HTTP backend permits only HTTP and HTTPS URLs",
        ));
    }

    let mut host_count = 0_usize;
    let mut content_length = None;
    let mut connection_policy = None;
    for header in request.headers() {
        if header.name().eq_ignore_ascii_case("host") {
            host_count += 1;
        } else if header.name().eq_ignore_ascii_case("content-length") {
            if upload.is_some() {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "NBReq generates Content-Length for a streamed upload body",
                ));
            }
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
            if upload.is_some() {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "NBReq generates Transfer-Encoding for a streamed upload body",
                ));
            }
            return Err(Error::new(
                ErrorKind::Unsupported,
                "buffered native requests do not support Transfer-Encoding",
            ));
        } else if header.name().eq_ignore_ascii_case("connection") {
            let parsed = parse_connection_policy(header.value())?;
            connection_policy = Some(match (connection_policy, parsed) {
                (None | Some(ConnectionPolicy::KeepAlive), ConnectionPolicy::KeepAlive) => {
                    ConnectionPolicy::KeepAlive
                }
                _ => ConnectionPolicy::Close,
            });
        }
    }
    if host_count > 1 {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "request contains more than one Host header",
        ));
    }
    if upload.is_none() && content_length.is_some_and(|length| length != request.body().len()) {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "request Content-Length does not match the buffered body",
        ));
    }

    let generated_host = host_count == 0;
    let generated_length = match upload {
        Some(UploadFraming::Fixed(_)) => true,
        Some(UploadFraming::Chunked) => false,
        None => content_length.is_none() && !request.body().is_empty(),
    };
    let generated_chunked = matches!(upload, Some(UploadFraming::Chunked));
    let generated_count = usize::from(generated_host)
        + usize::from(generated_length)
        + usize::from(generated_chunked);
    let field_count = request
        .headers()
        .len()
        .checked_add(generated_count)
        .ok_or_else(|| request_header_count_limit(limits.header_count))?;
    if field_count > limits.header_count {
        return Err(request_header_count_limit(limits.header_count));
    }

    let generated_length_value = match upload {
        Some(UploadFraming::Fixed(length)) => length.to_string(),
        Some(UploadFraming::Chunked) => String::new(),
        None => request.body().len().to_string(),
    };
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
        generated_chunked.then_some((b"Transfer-Encoding".as_slice(), b"chunked".as_slice())),
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
        .and_then(|bytes| {
            bytes.checked_add(if upload.is_none() {
                request.body().len()
            } else {
                0
            })
        })
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
    if generated_chunked {
        append_header(&mut bytes, b"Transfer-Encoding", b"chunked");
    }
    bytes.extend_from_slice(b"\r\n");
    if upload.is_none() {
        bytes.extend_from_slice(request.body());
    }
    Ok(SerializedRequest {
        bytes,
        response_to_head: matches!(request.method(), Method::Head),
        permits_reuse: connection_policy.is_none_or(|policy| policy == ConnectionPolicy::KeepAlive),
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
enum ConnectionPolicy {
    KeepAlive,
    Close,
}

fn parse_connection_policy(value: &[u8]) -> Result<ConnectionPolicy, Error> {
    let mut saw_token = false;
    let mut reusable = true;
    for raw in value.split(|byte| *byte == b',') {
        let token = trim_ascii_whitespace(raw);
        if token.is_empty()
            || !token
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(byte))
        {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "request Connection header contains an invalid token",
            ));
        }
        saw_token = true;
        reusable &= token.eq_ignore_ascii_case(b"keep-alive");
    }
    if !saw_token {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "request Connection header is empty",
        ));
    }
    Ok(if reusable {
        ConnectionPolicy::KeepAlive
    } else {
        ConnectionPolicy::Close
    })
}

fn trim_ascii_whitespace(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
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
    permits_reuse: bool,
}

#[derive(Debug)]
struct DecodeProgress {
    response: Option<Response>,
    consumed: usize,
    permits_reuse: bool,
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
            permits_reuse: false,
        }
    }

    fn ingest(&mut self, bytes: &[u8]) -> Result<DecodeProgress, Error> {
        if self.state == DecodeState::Complete {
            return Err(Error::new(
                ErrorKind::Internal,
                "native HTTP decoder received bytes after completion",
            ));
        }
        for (index, byte) in bytes.iter().enumerate() {
            if let Some(response) = self.consume(*byte)? {
                return Ok(DecodeProgress {
                    response: Some(response),
                    consumed: index + 1,
                    permits_reuse: self.permits_reuse,
                });
            }
        }
        Ok(DecodeProgress {
            response: None,
            consumed: bytes.len(),
            permits_reuse: false,
        })
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
                            permits_reuse,
                        } => {
                            self.status = Some(status);
                            self.headers = headers;
                            self.permits_reuse = permits_reuse;
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

#[allow(dead_code)] // Connected to NativeHttpBackend after its isolated framing gate.
struct StreamingResponseDecoder {
    limits: HttpLimits,
    response_to_head: bool,
    state: DecodeState,
    scratch: Vec<u8>,
    pending_head: Option<ResponseHead>,
    output: StreamOutput,
    response: Option<ResponseSink>,
    body_bytes: usize,
    informational_responses: u8,
    framing_bytes: usize,
    permits_reuse: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamOutput {
    AwaitHead,
    Deliver,
    Discard,
}

#[allow(dead_code)]
#[derive(Debug)]
enum StreamDecodeProgress {
    Head {
        head: ResponseHead,
        consumed: usize,
    },
    Body {
        consumed: usize,
        blocked: bool,
    },
    Complete {
        consumed: usize,
        permits_reuse: bool,
        delivered: bool,
    },
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StreamHeadDecision {
    complete: bool,
    permits_reuse: bool,
}

#[allow(dead_code)]
impl StreamingResponseDecoder {
    fn new(response_to_head: bool, limits: HttpLimits, response: ResponseSink) -> Self {
        Self {
            limits,
            response_to_head,
            state: DecodeState::Head,
            scratch: Vec::new(),
            pending_head: None,
            output: StreamOutput::AwaitHead,
            response: Some(response),
            body_bytes: 0,
            informational_responses: 0,
            framing_bytes: 0,
            permits_reuse: false,
        }
    }

    fn ingest(&mut self, bytes: &[u8]) -> Result<StreamDecodeProgress, Error> {
        if self.state == DecodeState::Complete {
            return Err(Error::transport(
                TransportStage::Http,
                "native streaming decoder received bytes after completion",
            ));
        }
        if self.pending_head.is_some() {
            return Err(Error::new(
                ErrorKind::Internal,
                "native streaming decoder advanced before its final head was decided",
            ));
        }

        let available = match self.output {
            StreamOutput::Deliver => self
                .response
                .as_ref()
                .map_or(0, ResponseSink::available_capacity),
            StreamOutput::AwaitHead | StreamOutput::Discard => usize::MAX,
        };
        let mut body = Vec::new();
        for (index, byte) in bytes.iter().copied().enumerate() {
            if self.output == StreamOutput::Deliver
                && self.next_byte_is_body()
                && body.len() == available
            {
                self.flush_body(body)?;
                return Ok(StreamDecodeProgress::Body {
                    consumed: index,
                    blocked: true,
                });
            }
            match self.consume_stream_byte(byte, &mut body)? {
                StreamByteProgress::Continue => {}
                StreamByteProgress::Head(head) => {
                    self.flush_body(body)?;
                    return Ok(StreamDecodeProgress::Head {
                        head,
                        consumed: index + 1,
                    });
                }
                StreamByteProgress::Complete => {
                    self.flush_body(body)?;
                    let delivered = self.output == StreamOutput::Deliver;
                    if delivered {
                        self.response_mut()?.complete();
                    }
                    return Ok(StreamDecodeProgress::Complete {
                        consumed: index + 1,
                        permits_reuse: self.permits_reuse,
                        delivered,
                    });
                }
            }
        }
        self.flush_body(body)?;
        Ok(StreamDecodeProgress::Body {
            consumed: bytes.len(),
            blocked: false,
        })
    }

    fn decide_head(&mut self, deliver: bool) -> Result<StreamHeadDecision, Error> {
        let head = self.pending_head.take().ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                "native streaming decoder has no final head to decide",
            )
        })?;
        self.output = if deliver {
            StreamOutput::Deliver
        } else {
            StreamOutput::Discard
        };
        let complete = matches!(self.state, DecodeState::Complete);
        if deliver && !self.response_mut()?.publish_head(head, complete) {
            return Err(Error::transport(
                TransportStage::Receive,
                "the streaming response reader closed before head publication",
            ));
        }
        Ok(StreamHeadDecision {
            complete,
            permits_reuse: self.permits_reuse,
        })
    }

    fn eof(&mut self) -> Result<Option<(bool, bool)>, Error> {
        if self.pending_head.is_some() {
            return Err(Error::new(
                ErrorKind::Internal,
                "native streaming response reached EOF before its final head was decided",
            ));
        }
        match self.state {
            DecodeState::CloseDelimited => {
                self.state = DecodeState::Complete;
                let delivered = self.output == StreamOutput::Deliver;
                if delivered {
                    self.response_mut()?.complete();
                }
                Ok(Some((self.permits_reuse, delivered)))
            }
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

    fn fail(&mut self, error: Error) {
        if let Some(response) = &mut self.response {
            response.fail(error);
        }
    }

    fn socket_capacity(&self) -> usize {
        if self.pending_head.is_some() {
            return 0;
        }
        match self.output {
            StreamOutput::AwaitHead | StreamOutput::Discard => 16 * 1024,
            StreamOutput::Deliver => self
                .response
                .as_ref()
                .map_or(0, ResponseSink::available_capacity),
        }
    }

    fn is_consumer_blocked(&self) -> bool {
        self.output == StreamOutput::Deliver && self.socket_capacity() == 0
    }

    fn into_response(mut self) -> Result<ResponseSink, Error> {
        self.take_response()
    }

    fn take_response(&mut self) -> Result<ResponseSink, Error> {
        if self.state != DecodeState::Complete || self.output != StreamOutput::Discard {
            return Err(Error::new(
                ErrorKind::Internal,
                "only a completely discarded streaming response can transfer its sink",
            ));
        }
        self.response.take().ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                "native streaming decoder response sink was already taken",
            )
        })
    }

    fn next_byte_is_body(&self) -> bool {
        matches!(
            self.state,
            DecodeState::Fixed { .. } | DecodeState::CloseDelimited | DecodeState::ChunkData { .. }
        )
    }

    fn consume_stream_byte(
        &mut self,
        byte: u8,
        body: &mut Vec<u8>,
    ) -> Result<StreamByteProgress, Error> {
        match self.state {
            DecodeState::Head => {
                self.push_stream_scratch(byte, "response head", false)?;
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
                            permits_reuse,
                        } => {
                            self.permits_reuse = permits_reuse;
                            self.state = match framing {
                                BodyFraming::None | BodyFraming::Fixed(0) => DecodeState::Complete,
                                BodyFraming::Fixed(remaining) => DecodeState::Fixed { remaining },
                                BodyFraming::Chunked => DecodeState::ChunkSize,
                                BodyFraming::CloseDelimited => DecodeState::CloseDelimited,
                            };
                            let head = ResponseHead::new(status, headers);
                            self.pending_head = Some(head.clone());
                            return Ok(StreamByteProgress::Head(head));
                        }
                    }
                }
            }
            DecodeState::Fixed { remaining } => {
                self.push_stream_body(byte, body)?;
                if remaining == 1 {
                    self.state = DecodeState::Complete;
                    return Ok(StreamByteProgress::Complete);
                }
                self.state = DecodeState::Fixed {
                    remaining: remaining - 1,
                };
            }
            DecodeState::CloseDelimited => self.push_stream_body(byte, body)?,
            DecodeState::ChunkSize => {
                self.push_stream_scratch(byte, "chunk-size line", true)?;
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
                    if size > self.limits.body_bytes.saturating_sub(self.body_bytes) {
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
                self.push_stream_body(byte, body)?;
                self.state = if remaining == 1 {
                    DecodeState::ChunkEnd { matched: 0 }
                } else {
                    DecodeState::ChunkData {
                        remaining: remaining - 1,
                    }
                };
            }
            DecodeState::ChunkEnd { matched: 0 } if byte == b'\r' => {
                self.count_stream_framing_byte("chunk terminator")?;
                self.state = DecodeState::ChunkEnd { matched: 1 };
            }
            DecodeState::ChunkEnd { matched: 1 } if byte == b'\n' => {
                self.count_stream_framing_byte("chunk terminator")?;
                self.state = DecodeState::ChunkSize;
            }
            DecodeState::ChunkEnd { .. } => {
                return Err(http_error("response chunk data is not followed by CRLF"));
            }
            DecodeState::Trailers => {
                self.push_stream_scratch(byte, "response trailers", true)?;
                if self.scratch == b"\r\n" || self.scratch.ends_with(b"\r\n\r\n") {
                    validate_trailers(&self.scratch, self.limits)?;
                    self.scratch.clear();
                    self.state = DecodeState::Complete;
                    return Ok(StreamByteProgress::Complete);
                }
            }
            DecodeState::Complete => {
                return Err(Error::transport(
                    TransportStage::Http,
                    "native streaming decoder advanced after completion",
                ));
            }
        }
        Ok(StreamByteProgress::Continue)
    }

    fn push_stream_scratch(&mut self, byte: u8, context: &str, framing: bool) -> Result<(), Error> {
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
            self.count_stream_framing_byte(context)?;
        }
        self.scratch.push(byte);
        Ok(())
    }

    fn count_stream_framing_byte(&mut self, context: &str) -> Result<(), Error> {
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

    fn push_stream_body(&mut self, byte: u8, body: &mut Vec<u8>) -> Result<(), Error> {
        if self.body_bytes >= self.limits.body_bytes {
            return Err(response_body_limit(self.limits.body_bytes));
        }
        self.body_bytes += 1;
        if self.output == StreamOutput::Deliver {
            body.push(byte);
        }
        Ok(())
    }

    fn flush_body(&mut self, body: Vec<u8>) -> Result<(), Error> {
        if body.is_empty() {
            return Ok(());
        }
        match self.response_mut()?.try_push(body) {
            Ok(()) => Ok(()),
            Err(ResponsePushError::WouldBlock(_)) => Err(Error::new(
                ErrorKind::Internal,
                "streaming decoder exceeded the capacity it observed",
            )),
            Err(ResponsePushError::Closed(_)) => Err(Error::transport(
                TransportStage::Receive,
                "the streaming response reader closed during delivery",
            )),
            Err(ResponsePushError::Protocol(_)) => Err(Error::new(
                ErrorKind::Internal,
                "streaming decoder delivered body before its final head",
            )),
            Err(ResponsePushError::Limit(_)) => Err(response_body_limit(self.limits.body_bytes)),
        }
    }

    fn response_mut(&mut self) -> Result<&mut ResponseSink, Error> {
        self.response.as_mut().ok_or_else(|| {
            Error::new(
                ErrorKind::Internal,
                "native streaming decoder no longer owns its response sink",
            )
        })
    }
}

#[derive(Clone, Debug)]
enum StreamByteProgress {
    Continue,
    Head(ResponseHead),
    Complete,
}

enum ParsedHead {
    Informational,
    Final {
        status: u16,
        headers: Vec<Header>,
        framing: BodyFraming,
        permits_reuse: bool,
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
    let stack_count = limits.header_count.min(STACK_RESPONSE_HEADER_SLOTS);
    let mut stack_slots = [httparse::EMPTY_HEADER; STACK_RESPONSE_HEADER_SLOTS];
    let parsed = match parse_response_parts(bytes, &mut stack_slots[..stack_count]) {
        Ok(parsed) => parsed,
        Err(ResponseHeadParseError::TooManyHeaders) if stack_count < limits.header_count => {
            let mut configured_slots = vec![httparse::EMPTY_HEADER; limits.header_count];
            parse_response_parts(bytes, &mut configured_slots).map_err(|error| match error {
                ResponseHeadParseError::TooManyHeaders => {
                    response_header_count_limit(limits.header_count)
                }
                ResponseHeadParseError::Invalid(error) => error,
            })?
        }
        Err(ResponseHeadParseError::TooManyHeaders) => {
            return Err(response_header_count_limit(limits.header_count));
        }
        Err(ResponseHeadParseError::Invalid(error)) => return Err(error),
    };
    let ParsedResponseParts {
        version,
        status,
        headers,
    } = parsed;
    if (100..200).contains(&status) {
        if status == 101 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "HTTP protocol switching is not supported",
            ));
        }
        return Ok(ParsedHead::Informational);
    }
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
    let permits_reuse = response_permits_reuse(version, &headers, framing)?;
    Ok(ParsedHead::Final {
        status,
        headers,
        framing,
        permits_reuse,
    })
}

struct ParsedResponseParts {
    version: u8,
    status: u16,
    headers: Vec<Header>,
}

enum ResponseHeadParseError {
    TooManyHeaders,
    Invalid(Error),
}

fn parse_response_parts<'bytes>(
    bytes: &'bytes [u8],
    slots: &mut [httparse::Header<'bytes>],
) -> Result<ParsedResponseParts, ResponseHeadParseError> {
    let mut parsed = httparse::Response::new(slots);
    let consumed = match parsed.parse(bytes) {
        Ok(httparse::Status::Complete(consumed)) => consumed,
        Ok(httparse::Status::Partial) => {
            return Err(ResponseHeadParseError::Invalid(http_error(
                "HTTP response head ended prematurely",
            )));
        }
        Err(httparse::Error::TooManyHeaders) => {
            return Err(ResponseHeadParseError::TooManyHeaders);
        }
        Err(error) => {
            return Err(ResponseHeadParseError::Invalid(http_error(format!(
                "HTTP response head is malformed: {error}"
            ))));
        }
    };
    if consumed != bytes.len() {
        return Err(ResponseHeadParseError::Invalid(http_error(
            "HTTP response head has trailing bytes",
        )));
    }
    let version = parsed.version.ok_or_else(|| {
        ResponseHeadParseError::Invalid(http_error("HTTP response has no version"))
    })?;
    if !matches!(version, 0 | 1) {
        return Err(ResponseHeadParseError::Invalid(http_error(
            "HTTP response version is unsupported",
        )));
    }
    let status = parsed
        .code
        .filter(|status| (100..=599).contains(status))
        .ok_or_else(|| {
            ResponseHeadParseError::Invalid(http_error("HTTP response status is invalid"))
        })?;
    let headers = if (100..200).contains(&status) {
        Vec::new()
    } else {
        parsed
            .headers
            .iter()
            .map(|header| Header::new(header.name, header.value.to_vec()))
            .collect()
    };
    Ok(ParsedResponseParts {
        version,
        status,
        headers,
    })
}

fn response_permits_reuse(
    version: u8,
    headers: &[Header],
    framing: BodyFraming,
) -> Result<bool, Error> {
    if matches!(framing, BodyFraming::CloseDelimited) {
        return Ok(false);
    }
    let mut close = false;
    let mut keep_alive = false;
    let mut other = false;
    for header in headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("connection"))
    {
        for raw in header.value().split(|byte| *byte == b',') {
            let token = trim_ascii_whitespace(raw);
            if token.is_empty()
                || !token
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(byte))
            {
                return Err(http_error(
                    "HTTP response Connection header contains an invalid token",
                ));
            }
            close |= token.eq_ignore_ascii_case(b"close");
            keep_alive |= token.eq_ignore_ascii_case(b"keep-alive");
            other |=
                !token.eq_ignore_ascii_case(b"close") && !token.eq_ignore_ascii_case(b"keep-alive");
        }
    }
    Ok(!close && !other && (version == 1 || keep_alive))
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

fn resolved_redirect_target(
    request: &Request,
    response: &Response,
) -> Result<Option<String>, Error> {
    resolved_redirect_target_from_headers(request, response.headers())
}

fn resolved_redirect_target_from_headers(
    request: &Request,
    headers: &[Header],
) -> Result<Option<String>, Error> {
    let mut location = None;
    for header in headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("location"))
    {
        if location.is_some() {
            return Err(Error::new(
                ErrorKind::Redirect,
                "a redirect response contained more than one Location field",
            ));
        }
        let value = header.value().trim_ascii_start().trim_ascii_end();
        location = Some(std::str::from_utf8(value).map_err(|_| {
            Error::new(
                ErrorKind::Redirect,
                "a redirect Location was not valid UTF-8",
            )
        })?);
    }
    let Some(location) = location else {
        return Ok(None);
    };
    let base = url::Url::parse(request.url()).map_err(|_| {
        Error::new(
            ErrorKind::Redirect,
            "the redirect source URL could not be resolved",
        )
    })?;
    let target = base.join(location).map_err(|_| {
        Error::new(
            ErrorKind::Redirect,
            "the redirect Location could not be resolved",
        )
    })?;
    Ok(Some(target.into()))
}

/// Native HTTP factory with the accepted DNS, TCP, TLS, redirect, pooling, and streaming owners.
/// Ordinary `Engine::new` selects this implementation when the default `native` feature is built.
#[allow(dead_code)]
pub(super) struct NativeHttpFactory {
    limits: HttpLimits,
    connection_limits: ConnectionLimits,
    resolver: Option<ResolverConfig>,
    tls: Option<NativeTlsConfigs>,
}

#[allow(dead_code)]
impl NativeHttpFactory {
    pub(super) fn new(config: &EngineConfig) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: None,
            tls: None,
        }
    }

    pub(super) fn new_with_nameserver(config: &EngineConfig, nameserver: SocketAddr) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: None,
        }
    }

    pub(super) fn new_with_nameserver_and_search_suffixes(
        config: &EngineConfig,
        nameserver: SocketAddr,
        suffixes: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver).with_search_suffixes(suffixes)),
            tls: None,
        }
    }

    pub(super) fn new_with_system_dns(config: &EngineConfig) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::system()?),
            tls: None,
        })
    }

    pub(super) fn new_with_nameserver_and_platform_tls(
        config: &EngineConfig,
        nameserver: SocketAddr,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: Some(NativeTlsConfigs::platform()?),
        })
    }

    pub(super) fn new_with_system_dns_and_platform_tls(
        config: &EngineConfig,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::system()?),
            tls: Some(NativeTlsConfigs::platform()?),
        })
    }

    pub(super) fn new_with_nameserver_and_test_root(
        config: &EngineConfig,
        nameserver: SocketAddr,
        root_der: Vec<u8>,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: Some(NativeTlsConfigs::with_test_root(root_der.into())?),
        })
    }

    #[cfg(test)]
    pub(super) fn new_with_nameserver_and_verification_gate(
        config: &EngineConfig,
        nameserver: SocketAddr,
        root_der: Vec<u8>,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
            connection_limits: ConnectionLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: Some(NativeTlsConfigs::with_test_root_and_verification_gate(
                root_der.into(),
                entered,
                release,
            )?),
        })
    }

    pub(super) fn into_backend(self) -> Result<Box<dyn Backend + Send>, Error> {
        Ok(Box::new(NativeHttpBackend::new(
            self.limits,
            self.resolver,
            self.tls,
            self.connection_limits,
        )?))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_write_limit(
        self,
        bytes: usize,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.reactor.limit_writes_for_test(bytes);
        Ok(Box::new(backend))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_failed_standalone_addresses(
        self,
        count: usize,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.failed_standalone_addresses_remaining = count;
        Ok(Box::new(backend))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_delayed_failed_standalone_address(
        self,
        delay: Duration,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.failed_standalone_addresses_remaining = 1;
        backend.failed_standalone_address_delay = Some(delay);
        Ok(Box::new(backend))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_standalone_dns_handoff_gate(
        self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.standalone_dns_handoff_gate = Some((entered, release));
        Ok(Box::new(backend))
    }

    #[cfg(test)]
    pub(super) fn into_backend_with_standalone_socket_gate(
        self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Result<Box<dyn Backend + Send>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.standalone_socket_gate = Some((entered, release));
        Ok(Box::new(backend))
    }
}

impl BackendFactory for NativeHttpFactory {
    fn connection_metrics_available(&self) -> bool {
        true
    }

    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error> {
        let mut backend =
            NativeHttpBackend::new(self.limits, self.resolver, self.tls, self.connection_limits)?;
        backend.attach_metrics(Arc::clone(&shared.metrics));
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

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_public_resolver(&self) -> bool {
        self.resolver.is_some()
    }

    fn supports_standalone_tcp(&self) -> bool {
        true
    }
}

struct HttpTransfer {
    request_id: RequestId,
    response: TransferResponse,
    body_bearing: bool,
    response_started: bool,
    connected: bool,
    tls: Option<NativeTls>,
    connect_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    inactivity_timeout: Option<Duration>,
    inactivity_deadline: Option<Instant>,
    inactivity_paused: bool,
    key: ConnectionKey,
    request_permits_reuse: bool,
    request_write_drained: bool,
    request: Request,
    redirect_hops: u8,
    upload: Option<NativeUpload>,
    upload_aborted: bool,
}

enum TransferResponse {
    Buffered(ResponseDecoder),
    Streaming {
        decoder: StreamingResponseDecoder,
        retained_cleartext: Vec<u8>,
        retained_offset: usize,
        peer_closed: bool,
        tls_dirty_eof: bool,
        redirect: Option<Request>,
    },
}

impl TransferResponse {
    fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming { .. })
    }
}

struct NativeUpload {
    body: UploadBody,
    framing: UploadFraming,
    producer_finished: bool,
    pending_wire: Option<Vec<u8>>,
}

enum UploadWire {
    Pending,
    Bytes(Vec<u8>),
    Complete,
}

impl NativeUpload {
    fn new(body: UploadBody) -> Self {
        let framing = body.framing();
        Self {
            body,
            framing,
            producer_finished: false,
            pending_wire: None,
        }
    }

    fn next_wire(&mut self, capacity: Option<usize>) -> Result<UploadWire, Error> {
        if self.pending_wire.is_none() && !self.producer_finished {
            self.prepare_wire()?;
        }
        if let Some(wire) = self.pending_wire.take() {
            if capacity.is_some_and(|capacity| wire.len() > capacity) {
                self.pending_wire = Some(wire);
                return Ok(UploadWire::Pending);
            }
            return Ok(UploadWire::Bytes(wire));
        }
        if self.producer_finished {
            Ok(UploadWire::Complete)
        } else {
            Ok(UploadWire::Pending)
        }
    }

    fn prepare_wire(&mut self) -> Result<(), Error> {
        match self.body.try_pop() {
            UploadPoll::Chunk(chunk) => match self.framing {
                UploadFraming::Fixed(_) => self.pending_wire = Some(chunk),
                UploadFraming::Chunked => {
                    let prefix = format!("{:X}\r\n", chunk.len());
                    let capacity = prefix
                        .len()
                        .checked_add(chunk.len())
                        .and_then(|length| length.checked_add(2))
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::Internal,
                                "native chunked upload framing length overflowed",
                            )
                        })?;
                    let mut wire = Vec::with_capacity(capacity);
                    wire.extend_from_slice(prefix.as_bytes());
                    wire.extend_from_slice(&chunk);
                    wire.extend_from_slice(b"\r\n");
                    self.pending_wire = Some(wire);
                }
            },
            UploadPoll::Pending => {}
            UploadPoll::Finished => match self.framing {
                UploadFraming::Fixed(_) => {
                    self.producer_finished = true;
                }
                UploadFraming::Chunked => {
                    self.producer_finished = true;
                    self.pending_wire = Some(b"0\r\n\r\n".to_vec());
                }
            },
            UploadPoll::Failed(error) => return Err(error),
        }
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.producer_finished && self.pending_wire.is_none()
    }

    fn close(&mut self) {
        self.body.close();
    }
}

fn cleartext_outbound_limit(serialized: &SerializedRequest, upload: Option<&UploadBody>) -> usize {
    upload.map_or(serialized.bytes.len(), |upload| {
        serialized
            .bytes
            .len()
            .saturating_add(upload.queue_capacity())
            .saturating_add(32)
    })
}

impl HttpTransfer {
    fn next_deadline(&self) -> Option<Instant> {
        [
            self.total_deadline,
            (!self.connected).then_some(self.connect_deadline).flatten(),
            (!self.inactivity_paused)
                .then_some(self.inactivity_deadline)
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn note_progress(&mut self, now: Instant, connected: bool) -> Option<Instant> {
        self.connected |= connected;
        self.inactivity_paused = false;
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
    tls: Option<NativeTlsConfigs>,
    limits: HttpLimits,
    request_to_slot: HashMap<RequestId, SlotId>,
    transfers: HashMap<SlotId, HttpTransfer>,
    request_to_resolve: HashMap<RequestId, ResolveKey>,
    resolves: HashMap<ResolveKey, PendingResolve>,
    request_to_public: HashMap<RequestId, ResolveKey>,
    public_lookups: HashMap<ResolveKey, PublicLookup>,
    pending_public: Vec<BackendResolveCompletion>,
    standalone_request_to_resolve: HashMap<RequestId, ResolveKey>,
    standalone_resolves: HashMap<ResolveKey, StandaloneResolve>,
    pending_http_from_dns: Vec<BackendCompletion>,
    next_resolve_key: u64,
    idle: HashMap<ConnectionKey, VecDeque<IdleConnection>>,
    idle_slots: HashMap<SlotId, ConnectionKey>,
    idle_count: usize,
    connection_count: usize,
    connections_per_key: HashMap<ConnectionKey, usize>,
    waiting: VecDeque<PendingResolve>,
    connection_limits: ConnectionLimits,
    metrics: Option<Arc<Metrics>>,
    standalone_request_to_slot: HashMap<RequestId, SlotId>,
    standalone_pending: HashMap<SlotId, StandalonePending>,
    standalone_live: HashMap<SlotId, StandaloneTcp>,
    #[cfg(test)]
    failed_standalone_addresses_remaining: usize,
    #[cfg(test)]
    failed_standalone_address_delay: Option<Duration>,
    #[cfg(test)]
    standalone_dns_handoff_gate:
        Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    #[cfg(test)]
    standalone_socket_gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}

struct StandaloneResolve {
    sink: TcpConnectSink,
    port: u16,
    connect_deadline: Option<Instant>,
}

struct StandalonePending {
    sink: TcpConnectSink,
    remaining: VecDeque<SocketAddr>,
    connect_deadline: Option<Instant>,
}

impl StandalonePending {
    fn id(&self) -> RequestId {
        self.sink.id()
    }

    fn is_terminal(&self) -> bool {
        self.sink.is_terminal()
    }

    fn connected(&self, local: SocketAddr, peer: SocketAddr) -> Option<TcpIoOwner> {
        self.sink.connected(local, peer)
    }

    fn read_inactivity_timeout(&self) -> Option<Duration> {
        self.sink.read_inactivity_timeout()
    }

    fn write_inactivity_timeout(&self) -> Option<Duration> {
        self.sink.write_inactivity_timeout()
    }
}

struct StandaloneTcp {
    request_id: RequestId,
    owner: TcpIoOwner,
    read_inactivity_timeout: Option<Duration>,
    read_inactivity_deadline: Option<Instant>,
    read_inactivity_paused: bool,
    write_inactivity_timeout: Option<Duration>,
    write_inactivity_deadline: Option<Instant>,
    write_inactivity_active: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StandaloneInactivity {
    Read,
    Write,
}

impl StandaloneTcp {
    fn new(
        request_id: RequestId,
        owner: TcpIoOwner,
        read_inactivity_timeout: Option<Duration>,
        write_inactivity_timeout: Option<Duration>,
        now: Instant,
    ) -> Self {
        Self {
            request_id,
            owner,
            read_inactivity_timeout,
            read_inactivity_deadline: read_inactivity_timeout
                .and_then(|timeout| now.checked_add(timeout)),
            read_inactivity_paused: false,
            write_inactivity_timeout,
            write_inactivity_deadline: None,
            write_inactivity_active: false,
        }
    }

    fn sync_pressure(&mut self, now: Instant) {
        if self.owner.read_allowance() == 0 {
            self.read_inactivity_paused = true;
            self.read_inactivity_deadline = None;
        } else if self.read_inactivity_paused {
            self.read_inactivity_paused = false;
            self.read_inactivity_deadline = self
                .read_inactivity_timeout
                .and_then(|timeout| now.checked_add(timeout));
        }

        let output_waiting = self.owner.send_occupancy() != 0;
        match (self.write_inactivity_active, output_waiting) {
            (false, true) => {
                self.write_inactivity_active = true;
                self.write_inactivity_deadline = self
                    .write_inactivity_timeout
                    .and_then(|timeout| now.checked_add(timeout));
            }
            (true, false) => {
                self.write_inactivity_active = false;
                self.write_inactivity_deadline = None;
            }
            _ => {}
        }
    }

    fn note_read_progress(&mut self, now: Instant) {
        if !self.read_inactivity_paused {
            self.read_inactivity_deadline = self
                .read_inactivity_timeout
                .and_then(|timeout| now.checked_add(timeout));
        }
    }

    fn note_write_progress(&mut self, now: Instant) {
        let output_waiting = self.owner.send_occupancy() != 0;
        self.write_inactivity_active = output_waiting;
        self.write_inactivity_deadline = output_waiting
            .then_some(self.write_inactivity_timeout)
            .flatten()
            .and_then(|timeout| now.checked_add(timeout));
    }

    fn note_peer_closed(&mut self) {
        self.read_inactivity_paused = true;
        self.read_inactivity_deadline = None;
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.read_inactivity_deadline,
            self.write_inactivity_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn expired(&self, now: Instant) -> Option<StandaloneInactivity> {
        if self
            .read_inactivity_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            Some(StandaloneInactivity::Read)
        } else if self
            .write_inactivity_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            Some(StandaloneInactivity::Write)
        } else {
            None
        }
    }
}

struct PublicLookup {
    request_id: RequestId,
    total_deadline: Option<Instant>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ConnectionKey {
    scheme: String,
    host: String,
    port: u16,
    dangerously_disable_tls_verification: bool,
}

struct IdleConnection {
    slot: SlotId,
    tls: Option<NativeTls>,
    expires_at: Instant,
}

struct PendingResolve {
    request_id: RequestId,
    serialized: SerializedRequest,
    port: u16,
    scheme: String,
    host: String,
    tls_verification: crate::TlsVerification,
    body_bearing: bool,
    connect_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    inactivity_timeout: Option<Duration>,
    inactivity_deadline: Option<Instant>,
    key: ConnectionKey,
    request: Request,
    redirect_hops: u8,
    response: PendingResponse,
}

enum PendingResponse {
    Buffered,
    Streaming {
        response: ResponseSink,
        upload: Option<UploadBody>,
    },
}

impl PendingResponse {
    fn upload(&self) -> Option<&UploadBody> {
        match self {
            Self::Buffered => None,
            Self::Streaming { upload, .. } => upload.as_ref(),
        }
    }

    fn fail(self, error: Error) -> Option<Completion> {
        match self {
            Self::Buffered => Some(Completion::Failed(error)),
            Self::Streaming {
                mut response,
                upload: _,
            } => {
                response.fail(error);
                None
            }
        }
    }

    fn into_active(
        self,
        response_to_head: bool,
        limits: HttpLimits,
    ) -> (TransferResponse, Option<NativeUpload>) {
        match self {
            Self::Buffered => (
                TransferResponse::Buffered(ResponseDecoder::new(response_to_head, limits)),
                None,
            ),
            Self::Streaming { response, upload } => (
                TransferResponse::Streaming {
                    decoder: StreamingResponseDecoder::new(response_to_head, limits, response),
                    retained_cleartext: Vec::new(),
                    retained_offset: 0,
                    peer_closed: false,
                    tls_dirty_eof: false,
                    redirect: None,
                },
                upload.map(NativeUpload::new),
            ),
        }
    }
}

#[derive(Clone, Copy)]
struct PendingDeadlines {
    connect: Option<Instant>,
    total: Option<Instant>,
    inactivity: Option<Instant>,
}

impl PendingResolve {
    fn fail(self, error: Error) -> Option<Completion> {
        self.response.fail(error)
    }

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
    fn new(
        limits: HttpLimits,
        resolver: Option<ResolverConfig>,
        tls: Option<NativeTlsConfigs>,
        connection_limits: ConnectionLimits,
    ) -> Result<Self, Error> {
        Self::new_with_connection_limits(limits, resolver, tls, connection_limits)
    }

    fn new_with_connection_limits(
        limits: HttpLimits,
        resolver: Option<ResolverConfig>,
        tls: Option<NativeTlsConfigs>,
        connection_limits: ConnectionLimits,
    ) -> Result<Self, Error> {
        if connection_limits.global == 0 || connection_limits.per_origin == 0 {
            return Err(Error::new(
                ErrorKind::Internal,
                "native connection limits are invalid",
            ));
        }
        let reactor = NativeReactor::new(256).map_err(native_internal_error)?;
        let resolver = resolver
            .map(|config| NativeResolver::new(config, reactor.waker()))
            .transpose()?;
        Ok(Self {
            reactor,
            resolver,
            tls,
            limits,
            request_to_slot: HashMap::new(),
            transfers: HashMap::new(),
            request_to_resolve: HashMap::new(),
            resolves: HashMap::new(),
            request_to_public: HashMap::new(),
            public_lookups: HashMap::new(),
            pending_public: Vec::new(),
            standalone_request_to_resolve: HashMap::new(),
            standalone_resolves: HashMap::new(),
            pending_http_from_dns: Vec::new(),
            next_resolve_key: 1,
            idle: HashMap::new(),
            idle_slots: HashMap::new(),
            idle_count: 0,
            connection_count: 0,
            connections_per_key: HashMap::new(),
            waiting: VecDeque::new(),
            connection_limits,
            metrics: None,
            standalone_request_to_slot: HashMap::new(),
            standalone_pending: HashMap::new(),
            standalone_live: HashMap::new(),
            #[cfg(test)]
            failed_standalone_addresses_remaining: 0,
            #[cfg(test)]
            failed_standalone_address_delay: None,
            #[cfg(test)]
            standalone_dns_handoff_gate: None,
            #[cfg(test)]
            standalone_socket_gate: None,
        })
    }

    fn can_reserve_connection(&self, key: &ConnectionKey) -> bool {
        self.connection_count < self.connection_limits.global
            && self.connections_per_key.get(key).copied().unwrap_or(0)
                < self.connection_limits.per_origin
    }

    fn reserve_connection(&mut self, key: &ConnectionKey) -> bool {
        if !self.can_reserve_connection(key) {
            return false;
        }
        self.connection_count += 1;
        *self.connections_per_key.entry(key.clone()).or_default() += 1;
        if let Some(metrics) = &self.metrics {
            metrics.connection_opened(self.connection_count);
        }
        true
    }

    fn release_connection(&mut self, key: &ConnectionKey) {
        let Some(count) = self.connections_per_key.get_mut(key) else {
            debug_assert!(false, "released an unreserved native connection");
            return;
        };
        if self.connection_count == 0 || *count == 0 {
            debug_assert!(false, "native connection reservation count underflowed");
            return;
        }
        self.connection_count -= 1;
        *count -= 1;
        if *count == 0 {
            self.connections_per_key.remove(key);
        }
        if let Some(metrics) = &self.metrics {
            metrics.connection_closed(self.connection_count);
        }
    }

    fn evict_idle_for_key(&mut self, key: &ConnectionKey) -> bool {
        let slot = self
            .idle
            .get(key)
            .and_then(|connections| connections.front())
            .map(|connection| connection.slot);
        if let Some(slot) = slot {
            self.discard_idle_slot(slot);
            true
        } else {
            false
        }
    }

    fn evict_any_idle(&mut self) -> bool {
        let slot = self
            .idle
            .values()
            .find_map(|connections| connections.front())
            .map(|connection| connection.slot);
        if let Some(slot) = slot {
            self.discard_idle_slot(slot);
            true
        } else {
            false
        }
    }

    fn make_connection_capacity(&mut self, key: &ConnectionKey) -> bool {
        while self.connections_per_key.get(key).copied().unwrap_or(0)
            >= self.connection_limits.per_origin
        {
            if !self.evict_idle_for_key(key) {
                return false;
            }
        }
        while self.connection_count >= self.connection_limits.global {
            if !self.evict_any_idle() {
                return false;
            }
        }
        true
    }

    fn expire_waiting(&mut self) -> Vec<BackendCompletion> {
        let now = Instant::now();
        let mut completions = Vec::new();
        let mut index = 0;
        while index < self.waiting.len() {
            if self.waiting[index]
                .next_deadline()
                .is_none_or(|deadline| deadline > now)
            {
                index += 1;
                continue;
            }
            let Some(pending) = self.waiting.remove(index) else {
                break;
            };
            let timeout = pending.expired_timeout(now);
            let id = pending.request_id;
            if let Some(completion) = pending
                .response
                .fail(Error::timeout(timeout, native_timeout_message(timeout)))
            {
                completions.push(BackendCompletion { id, completion });
            }
        }
        if let Some(metrics) = &self.metrics {
            metrics.set_connection_waiters(self.waiting.len());
        }
        completions
    }

    fn dispatch_waiting(&mut self) -> Vec<BackendCompletion> {
        let mut completions = self.expire_waiting();
        let mut index = 0;
        while index < self.waiting.len() {
            let Some(pending) = self.waiting.remove(index) else {
                break;
            };
            let Some(pending) = self.try_begin_reused(pending) else {
                index = 0;
                continue;
            };
            if !self.make_connection_capacity(&pending.key)
                || !self.reserve_connection(&pending.key)
            {
                self.waiting.insert(index, pending);
                index += 1;
                continue;
            }
            let id = pending.request_id;
            if let Some(completion) = self.start_reserved(pending) {
                completions.push(BackendCompletion { id, completion });
            }
            index = 0;
        }
        if let Some(metrics) = &self.metrics {
            metrics.set_connection_waiters(self.waiting.len());
        }
        completions
    }

    fn make_pending(
        &self,
        request_id: RequestId,
        request: Request,
        deadlines: PendingDeadlines,
        redirect_hops: u8,
        origin_error_kind: ErrorKind,
        response: PendingResponse,
    ) -> Result<PendingResolve, (Error, PendingResponse)> {
        let upload_framing = response.upload().map(UploadBody::framing);
        let serialized = match serialize_request_with_upload(&request, self.limits, upload_framing)
        {
            Ok(serialized) => serialized,
            Err(error) => return Err((error, response)),
        };
        let origin = match http_origin(request.url(), origin_error_kind) {
            Ok(origin) => origin,
            Err(error) => return Err((error, response)),
        };
        let tls_verification = request.options().tls_verification;
        let inactivity_timeout = request.options().inactivity_timeout;
        let body_bearing = response.upload().is_some() || !request.body().is_empty();
        let key = ConnectionKey {
            scheme: origin.scheme.clone(),
            host: origin.host.to_ascii_lowercase(),
            port: origin.port,
            dangerously_disable_tls_verification: matches!(
                tls_verification,
                crate::TlsVerification::DangerouslyDisableCertificateVerification
            ),
        };
        Ok(PendingResolve {
            request_id,
            serialized,
            port: origin.port,
            scheme: origin.scheme,
            host: origin.host,
            tls_verification,
            body_bearing,
            connect_deadline: deadlines.connect,
            total_deadline: deadlines.total,
            inactivity_timeout,
            inactivity_deadline: deadlines.inactivity,
            key,
            request,
            redirect_hops,
            response,
        })
    }

    fn start_pending(&mut self, pending: PendingResolve) -> Option<Completion> {
        let now = Instant::now();
        if pending
            .next_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            let timeout = pending.expired_timeout(now);
            return pending.fail(Error::timeout(timeout, native_timeout_message(timeout)));
        }
        let pending = self.try_begin_reused(pending)?;
        if !self.make_connection_capacity(&pending.key) || !self.reserve_connection(&pending.key) {
            self.waiting.push_back(pending);
            if let Some(metrics) = &self.metrics {
                metrics.set_connection_waiters(self.waiting.len());
            }
            return None;
        }
        self.start_reserved(pending)
    }

    fn finish(
        &mut self,
        slot: SlotId,
        completion: Completion,
        completions: &mut Vec<BackendCompletion>,
    ) {
        self.reactor.cancel(slot);
        if let Some(mut transfer) = self.transfers.remove(&slot) {
            self.request_to_slot.remove(&transfer.request_id);
            self.release_connection(&transfer.key);
            match &mut transfer.response {
                TransferResponse::Buffered(_) => completions.push(BackendCompletion {
                    id: transfer.request_id,
                    completion,
                }),
                TransferResponse::Streaming { decoder, .. } => match completion {
                    Completion::Failed(error) => decoder.fail(error),
                    Completion::Cancelled => decoder.fail(Error::new(
                        ErrorKind::Internal,
                        "native streaming transport cancelled without registry arbitration",
                    )),
                    Completion::Completed(_) => decoder.fail(Error::new(
                        ErrorKind::Internal,
                        "native streaming transport produced a buffered completion",
                    )),
                },
            }
        }
    }

    fn take_idle(&mut self, key: &ConnectionKey) -> Option<IdleConnection> {
        let connection = self.idle.get_mut(key)?.pop_front()?;
        if self.idle.get(key).is_some_and(VecDeque::is_empty) {
            self.idle.remove(key);
        }
        self.idle_slots.remove(&connection.slot);
        self.idle_count = self.idle_count.saturating_sub(1);
        if let Some(metrics) = &self.metrics {
            metrics.set_idle_connections(self.idle_count);
        }
        Some(connection)
    }

    fn discard_idle_slot(&mut self, slot: SlotId) {
        let Some(key) = self.idle_slots.remove(&slot) else {
            self.reactor.cancel(slot);
            return;
        };
        if let Some(connections) = self.idle.get_mut(&key) {
            if let Some(index) = connections
                .iter()
                .position(|connection| connection.slot == slot)
            {
                connections.remove(index);
                self.idle_count = self.idle_count.saturating_sub(1);
            }
            if connections.is_empty() {
                self.idle.remove(&key);
            }
        }
        if let Some(metrics) = &self.metrics {
            metrics.set_idle_connections(self.idle_count);
            metrics.idle_evicted();
        }
        self.reactor.cancel(slot);
        self.release_connection(&key);
    }

    fn expire_idle(&mut self, now: Instant) {
        let expired = self
            .idle
            .values()
            .flat_map(|connections| connections.iter())
            .filter(|connection| connection.expires_at <= now)
            .map(|connection| connection.slot)
            .collect::<Vec<_>>();
        for slot in expired {
            self.discard_idle_slot(slot);
        }
    }

    fn try_begin_reused(&mut self, pending: PendingResolve) -> Option<PendingResolve> {
        let Some(mut idle) = self.take_idle(&pending.key) else {
            return Some(pending);
        };
        if self.reactor.idle_is_quiet(idle.slot) != Ok(true) {
            self.reactor.cancel(idle.slot);
            self.release_connection(&pending.key);
            return Some(pending);
        }
        let deadline = pending.next_deadline();
        let outbound_limit = if idle.tls.is_some() {
            encrypted_outbound_limit()
        } else {
            cleartext_outbound_limit(&pending.serialized, pending.response.upload())
        };
        let receive_limit = if idle.tls.is_some() {
            encrypted_receive_limit(self.limits.reactor_receive_limit())
        } else {
            self.limits.reactor_receive_limit()
        };
        if self
            .reactor
            .prepare_reuse(idle.slot, deadline, outbound_limit, receive_limit)
            .is_err()
        {
            self.reactor.cancel(idle.slot);
            self.release_connection(&pending.key);
            return Some(pending);
        }
        let outbound = match idle.tls.as_mut() {
            Some(tls) => {
                if tls.begin_request(pending.serialized.bytes.clone()).is_err() {
                    self.reactor.cancel(idle.slot);
                    self.release_connection(&pending.key);
                    return Some(pending);
                }
                match tls.pump_request(outbound_limit) {
                    Ok(outbound) => outbound,
                    Err(_) => {
                        self.reactor.cancel(idle.slot);
                        self.release_connection(&pending.key);
                        return Some(pending);
                    }
                }
            }
            None => pending.serialized.bytes.clone(),
        };
        if self.reactor.queue_write(idle.slot, &outbound).is_err() {
            self.reactor.cancel(idle.slot);
            self.release_connection(&pending.key);
            return Some(pending);
        }
        let request_id = pending.request_id;
        let (response, upload) = pending
            .response
            .into_active(pending.serialized.response_to_head, self.limits);
        self.request_to_slot.insert(request_id, idle.slot);
        self.transfers.insert(
            idle.slot,
            HttpTransfer {
                request_id,
                response,
                body_bearing: pending.body_bearing,
                response_started: false,
                connected: true,
                tls: idle.tls,
                connect_deadline: pending.connect_deadline,
                total_deadline: pending.total_deadline,
                inactivity_timeout: pending.inactivity_timeout,
                inactivity_deadline: pending.inactivity_deadline,
                inactivity_paused: false,
                key: pending.key,
                request_permits_reuse: pending.serialized.permits_reuse,
                request_write_drained: false,
                request: pending.request,
                redirect_hops: pending.redirect_hops,
                upload,
                upload_aborted: false,
            },
        );
        if let Some(metrics) = &self.metrics {
            metrics.connection_reused();
        }
        None
    }

    fn complete_response(
        &mut self,
        slot: SlotId,
        response: Response,
        response_permits_reuse: bool,
        peer_closed: bool,
        completions: &mut Vec<BackendCompletion>,
    ) {
        let Some(transfer) = self.transfers.remove(&slot) else {
            self.reactor.cancel(slot);
            return;
        };
        self.request_to_slot.remove(&transfer.request_id);
        let redirect = redirected_request(
            &transfer.request,
            response.status(),
            transfer.redirect_hops,
            || resolved_redirect_target(&transfer.request, &response),
        );
        let request_id = transfer.request_id;
        let total_deadline = transfer.total_deadline;
        let next_redirect_hops = transfer.redirect_hops.saturating_add(1);
        let per_origin_idle = self.idle.get(&transfer.key).map_or(0, VecDeque::len);
        let reusable = response_permits_reuse
            && transfer.request_permits_reuse
            && transfer.request_write_drained
            && !peer_closed
            && self.connection_limits.idle_timeout != Duration::ZERO
            && self.idle_count < self.connection_limits.idle_global
            && per_origin_idle < self.connection_limits.idle_per_origin;
        if reusable {
            let parked = Instant::now()
                .checked_add(self.connection_limits.idle_timeout)
                .filter(|idle_deadline| {
                    self.reactor
                        .set_deadline(slot, Some(*idle_deadline))
                        .is_ok()
                });
            if let Some(expires_at) = parked {
                self.idle_slots.insert(slot, transfer.key.clone());
                self.idle
                    .entry(transfer.key)
                    .or_default()
                    .push_back(IdleConnection {
                        slot,
                        tls: transfer.tls,
                        expires_at,
                    });
                self.idle_count += 1;
                if let Some(metrics) = &self.metrics {
                    metrics.set_idle_connections(self.idle_count);
                }
            } else {
                self.reactor.cancel(slot);
                self.release_connection(&transfer.key);
            }
        } else {
            self.reactor.cancel(slot);
            self.release_connection(&transfer.key);
        }
        match redirect {
            Ok(None) => completions.push(BackendCompletion {
                id: request_id,
                completion: Completion::Completed(response),
            }),
            Err(error) => completions.push(BackendCompletion {
                id: request_id,
                completion: Completion::Failed(error),
            }),
            Ok(Some(request)) => {
                let now = Instant::now();
                let connect_deadline = request
                    .options()
                    .connect_timeout
                    .and_then(|timeout| now.checked_add(timeout));
                let inactivity_deadline = request
                    .options()
                    .inactivity_timeout
                    .and_then(|timeout| now.checked_add(timeout));
                match self.make_pending(
                    request_id,
                    request,
                    PendingDeadlines {
                        connect: connect_deadline,
                        total: total_deadline,
                        inactivity: inactivity_deadline,
                    },
                    next_redirect_hops,
                    ErrorKind::Redirect,
                    PendingResponse::Buffered,
                ) {
                    Ok(pending) => {
                        if let Some(completion) = self.start_pending(pending) {
                            completions.push(BackendCompletion {
                                id: request_id,
                                completion,
                            });
                        }
                    }
                    Err((error, _response)) => completions.push(BackendCompletion {
                        id: request_id,
                        completion: Completion::Failed(error),
                    }),
                }
            }
        }
    }

    fn begin_connection(
        &mut self,
        address: SocketAddr,
        mut pending: PendingResolve,
    ) -> Option<Completion> {
        let deadline = pending.next_deadline();
        let tls = if pending.scheme == "https" {
            let Some(configs) = &self.tls else {
                self.release_connection(&pending.key);
                return pending.fail(Error::new(
                    ErrorKind::Unsupported,
                    "the selected native proving Engine has no TLS configuration",
                ));
            };
            match configs.connection(
                &pending.host,
                pending.tls_verification,
                pending.serialized.bytes.clone(),
            ) {
                Ok(tls) => Some(tls),
                Err(error) => {
                    self.release_connection(&pending.key);
                    return pending.fail(error);
                }
            }
        } else {
            None
        };
        let outbound_limit = if tls.is_some() {
            encrypted_outbound_limit()
        } else {
            cleartext_outbound_limit(&pending.serialized, pending.response.upload())
        };
        let receive_limit = if tls.is_some() {
            encrypted_receive_limit(self.limits.reactor_receive_limit())
        } else {
            self.limits.reactor_receive_limit()
        };
        let slot = match self
            .reactor
            .connect(address, deadline, outbound_limit, receive_limit)
        {
            Ok(slot) => slot,
            Err(failure) => {
                self.release_connection(&pending.key);
                return pending.fail(native_transport_error(failure));
            }
        };
        if tls.is_none() {
            if let Err(failure) = self.reactor.queue_write(slot, &pending.serialized.bytes) {
                self.reactor.cancel(slot);
                self.release_connection(&pending.key);
                return pending.fail(native_transport_error(failure));
            }
        }
        let request_id = pending.request_id;
        let now = Instant::now();
        pending.inactivity_deadline = pending
            .inactivity_timeout
            .and_then(|timeout| now.checked_add(timeout));
        let (response, upload) = pending
            .response
            .into_active(pending.serialized.response_to_head, self.limits);
        self.request_to_slot.insert(request_id, slot);
        self.transfers.insert(
            slot,
            HttpTransfer {
                request_id,
                response,
                body_bearing: pending.body_bearing,
                response_started: false,
                connected: false,
                tls,
                connect_deadline: pending.connect_deadline,
                total_deadline: pending.total_deadline,
                inactivity_timeout: pending.inactivity_timeout,
                inactivity_deadline: pending.inactivity_deadline,
                inactivity_paused: false,
                key: pending.key,
                request_permits_reuse: pending.serialized.permits_reuse,
                request_write_drained: false,
                request: pending.request,
                redirect_hops: pending.redirect_hops,
                upload,
                upload_aborted: false,
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
            if let Some(public) = result.public {
                if let Some(lookup) = self.standalone_resolves.remove(&result.key) {
                    self.standalone_request_to_resolve.remove(&lookup.sink.id());
                    self.finish_standalone_resolve(lookup, public);
                    continue;
                }
                let Some(lookup) = self.public_lookups.remove(&result.key) else {
                    continue;
                };
                self.request_to_public.remove(&lookup.request_id);
                if lookup
                    .total_deadline
                    .is_some_and(|deadline| deadline <= Instant::now())
                {
                    if let Some(resolver) = &self.resolver {
                        resolver.cancel(result.key)?;
                    }
                    self.pending_public.push(BackendResolveCompletion {
                        id: lookup.request_id,
                        completion: public_total_timeout(),
                    });
                    continue;
                }
                self.pending_public.push(BackendResolveCompletion {
                    id: lookup.request_id,
                    completion: match public {
                        PublicLookupOutcome::Completed {
                            name,
                            status,
                            addresses,
                            valid_until,
                            from_cache,
                            candidate_name,
                        } => ResolveCompletion::Completed(ResolveResponse::new(
                            name,
                            status,
                            addresses.into_iter().map(ResolvedAddress::new).collect(),
                            valid_until,
                            from_cache,
                            candidate_name,
                        )),
                        PublicLookupOutcome::Failed(error) => ResolveCompletion::Failed(error),
                    },
                });
                continue;
            }
            let Some(pending) = self.resolves.remove(&result.key) else {
                continue;
            };
            self.request_to_resolve.remove(&pending.request_id);
            match result.result {
                Ok(answer) => {
                    let Some(ip) = answer.addresses.into_iter().next() else {
                        self.release_connection(&pending.key);
                        let id = pending.request_id;
                        if let Some(completion) = pending.fail(Error::transport(
                            TransportStage::Dns,
                            "the native resolver returned no usable address",
                        )) {
                            completions.push(BackendCompletion { id, completion });
                        }
                        continue;
                    };
                    let id = pending.request_id;
                    if let Some(completion) =
                        self.begin_connection(SocketAddr::new(ip, pending.port), pending)
                    {
                        completions.push(BackendCompletion { id, completion });
                    }
                }
                Err(failure) => {
                    self.release_connection(&pending.key);
                    let id = pending.request_id;
                    if let Some(completion) =
                        pending.fail(Error::transport(TransportStage::Dns, failure.message))
                    {
                        completions.push(BackendCompletion { id, completion });
                    }
                }
            }
        }
        Ok(completions)
    }

    fn finish_standalone_resolve(
        &mut self,
        lookup: StandaloneResolve,
        outcome: PublicLookupOutcome,
    ) {
        if lookup
            .connect_deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            lookup.sink.fail(standalone_connect_timeout());
            return;
        }
        if !lookup.sink.release_dns_borrow() {
            return;
        }
        #[cfg(test)]
        if let Some((entered, release)) = self.standalone_dns_handoff_gate.take() {
            entered.send(()).expect("observe standalone DNS handoff");
            release.recv().expect("release standalone DNS handoff");
        }
        if lookup.sink.is_terminal() {
            return;
        }
        match outcome {
            PublicLookupOutcome::Completed {
                status: ResolveStatus::Answer,
                addresses,
                ..
            } if !addresses.is_empty() => {
                let remaining = addresses
                    .into_iter()
                    .map(|address| SocketAddr::new(address, lookup.port))
                    .collect();
                self.start_standalone_attempt(StandalonePending {
                    sink: lookup.sink,
                    remaining,
                    connect_deadline: lookup.connect_deadline,
                });
            }
            PublicLookupOutcome::Completed { status, .. } => {
                let message = match status {
                    ResolveStatus::NameNotFound => "the standalone TCP hostname does not exist",
                    ResolveStatus::NoData => {
                        "the standalone TCP hostname has no usable address data"
                    }
                    ResolveStatus::Answer => {
                        "the standalone TCP hostname resolved without a usable address"
                    }
                };
                lookup
                    .sink
                    .fail(Error::transport(TransportStage::Dns, message));
            }
            PublicLookupOutcome::Failed(error) => {
                lookup.sink.fail(error);
            }
        }
    }

    fn expire_public_lookups(&mut self) -> Result<(), Error> {
        let now = Instant::now();
        let expired = self
            .public_lookups
            .iter()
            .filter_map(|(key, lookup)| {
                lookup
                    .total_deadline
                    .is_some_and(|deadline| deadline <= now)
                    .then_some(*key)
            })
            .collect::<Vec<_>>();
        for key in expired {
            let Some(lookup) = self.public_lookups.remove(&key) else {
                continue;
            };
            self.request_to_public.remove(&lookup.request_id);
            if let Some(resolver) = &self.resolver {
                resolver.cancel(key)?;
            }
            self.pending_public.push(BackendResolveCompletion {
                id: lookup.request_id,
                completion: public_total_timeout(),
            });
        }
        Ok(())
    }

    fn expire_standalone_resolves(&mut self) -> Result<(), Error> {
        let now = Instant::now();
        let finished = self
            .standalone_resolves
            .iter()
            .filter_map(|(key, lookup)| {
                (lookup.sink.is_terminal()
                    || lookup
                        .connect_deadline
                        .is_some_and(|deadline| deadline <= now))
                .then_some(*key)
            })
            .collect::<Vec<_>>();
        for key in finished {
            let Some(lookup) = self.standalone_resolves.remove(&key) else {
                continue;
            };
            self.standalone_request_to_resolve.remove(&lookup.sink.id());
            if let Some(resolver) = &self.resolver {
                resolver.cancel(key)?;
            }
            if !lookup.sink.is_terminal() {
                lookup.sink.fail(standalone_connect_timeout());
            }
        }
        Ok(())
    }

    fn drain_dns(&mut self) -> Result<(), Error> {
        self.expire_public_lookups()?;
        self.expire_standalone_resolves()?;
        let http = self.process_resolver_results()?;
        self.pending_http_from_dns.extend(http);
        Ok(())
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
            self.release_connection(&pending.key);
            if let Some(resolver) = &self.resolver {
                resolver.cancel(key)?;
            }
            let timeout = pending.expired_timeout(now);
            let id = pending.request_id;
            if let Some(completion) = pending
                .response
                .fail(Error::timeout(timeout, native_timeout_message(timeout)))
            {
                completions.push(BackendCompletion { id, completion });
            }
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

    fn start_standalone_attempt(&mut self, mut pending: StandalonePending) {
        loop {
            if pending.is_terminal() {
                return;
            }
            if pending
                .connect_deadline
                .is_some_and(|deadline| deadline <= Instant::now())
            {
                pending.sink.fail(standalone_connect_timeout());
                return;
            }
            let Some(address) = pending.remaining.pop_front() else {
                pending.sink.fail(Error::transport(
                    TransportStage::Connect,
                    "every resolved standalone TCP address failed to connect",
                ));
                return;
            };
            #[cfg(test)]
            if self.failed_standalone_addresses_remaining != 0 {
                self.failed_standalone_addresses_remaining -= 1;
                if let Some(delay) = self.failed_standalone_address_delay.take() {
                    std::thread::sleep(delay);
                }
                if pending.remaining.is_empty() {
                    pending.sink.fail(Error::transport(
                        TransportStage::Connect,
                        "the injected standalone TCP address failed to connect",
                    ));
                    return;
                }
                continue;
            }
            let slot = match self.reactor.connect(
                address,
                pending.connect_deadline,
                pending.sink.send_window(),
                usize::MAX,
            ) {
                Ok(slot) => slot,
                Err(failure)
                    if failure.kind == NativeFailureKind::Connect
                        && !pending.remaining.is_empty() =>
                {
                    continue;
                }
                Err(failure) => {
                    pending.sink.fail(native_transport_error(failure));
                    return;
                }
            };
            if let Err(failure) = self
                .reactor
                .set_read_allowance(slot, Some(pending.sink.receive_window()))
            {
                self.reactor.cancel(slot);
                pending.sink.fail(native_internal_error(failure));
                return;
            }
            if pending.is_terminal() {
                self.reactor.cancel(slot);
                return;
            }
            self.standalone_request_to_slot.insert(pending.id(), slot);
            self.standalone_pending.insert(slot, pending);
            #[cfg(test)]
            if let Some((entered, release)) = self.standalone_socket_gate.take() {
                entered
                    .send(())
                    .expect("observe owned standalone socket attempt");
                release
                    .recv()
                    .expect("release owned standalone socket attempt");
            }
            return;
        }
    }

    fn start_reserved(&mut self, pending: PendingResolve) -> Option<Completion> {
        if let Ok(ip) = pending.host.parse::<IpAddr>() {
            return self.begin_connection(SocketAddr::new(ip, pending.port), pending);
        }
        let key = match self.next_resolve_key() {
            Ok(key) => key,
            Err(error) => {
                self.release_connection(&pending.key);
                return pending.fail(error);
            }
        };
        let Some(resolver) = &self.resolver else {
            self.release_connection(&pending.key);
            return pending.fail(Error::new(
                ErrorKind::Unsupported,
                "the native HTTP proving Engine requires an injected resolver for hostnames",
            ));
        };
        if let Err(error) = resolver.resolve(key, pending.host.clone()) {
            self.release_connection(&pending.key);
            return pending.fail(error);
        }
        self.request_to_resolve.insert(pending.request_id, key);
        self.resolves.insert(key, pending);
        None
    }

    fn fail_native(
        &mut self,
        slot: SlotId,
        failure: NativeFailure,
        completions: &mut Vec<BackendCompletion>,
    ) {
        let tls_handshake = self
            .transfers
            .get(&slot)
            .and_then(|transfer| transfer.tls.as_ref())
            .is_some_and(NativeTls::is_handshaking);
        let error = if tls_handshake {
            Error::tls(
                TransportStage::Tls,
                TlsFailure::Io,
                format!(
                    "the connection failed during the TLS handshake: {}",
                    failure.message
                ),
            )
        } else if failure.kind == NativeFailureKind::Read
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

    fn pump_tls_request(
        &mut self,
        slot: SlotId,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<bool, Error> {
        let capacity = match self.reactor.outbound_capacity(slot) {
            Ok(capacity) => capacity,
            Err(failure) => {
                self.finish(
                    slot,
                    Completion::Failed(Error::new(ErrorKind::Internal, failure.message)),
                    completions,
                );
                return Ok(false);
            }
        };
        let pumped = self
            .transfers
            .get_mut(&slot)
            .and_then(|transfer| transfer.tls.as_mut())
            .map(|tls| tls.pump_request(capacity));
        let outbound = match pumped {
            Some(Ok(outbound)) => outbound,
            Some(Err(error)) => {
                self.finish(slot, Completion::Failed(error), completions);
                return Ok(false);
            }
            None => return Ok(false),
        };
        let queued = !outbound.is_empty();
        if queued {
            if let Err(failure) = self.reactor.queue_write(slot, &outbound) {
                self.finish(
                    slot,
                    Completion::Failed(Error::transport(TransportStage::Send, failure.message)),
                    completions,
                );
                return Ok(false);
            }
        }
        Ok(queued)
    }

    fn pump_request_output(
        &mut self,
        slot: SlotId,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<bool, Error> {
        let mut queued = self.pump_tls_request(slot, completions)?;
        loop {
            let Some(uses_tls) = self
                .transfers
                .get(&slot)
                .map(|transfer| transfer.tls.is_some())
            else {
                return Ok(queued);
            };
            if self
                .transfers
                .get(&slot)
                .is_none_or(|transfer| transfer.upload.is_none())
            {
                return Ok(queued);
            }

            if uses_tls {
                let ready = self
                    .transfers
                    .get(&slot)
                    .and_then(|transfer| transfer.tls.as_ref())
                    .is_some_and(|tls| !tls.is_handshaking() && tls.request_fully_encrypted());
                if !ready {
                    return Ok(queued);
                }
                let next = self
                    .transfers
                    .get_mut(&slot)
                    .and_then(|transfer| transfer.upload.as_mut())
                    .expect("streamed upload presence checked")
                    .next_wire(None);
                match next {
                    Ok(UploadWire::Bytes(wire)) => {
                        let begun = self
                            .transfers
                            .get_mut(&slot)
                            .and_then(|transfer| transfer.tls.as_mut())
                            .expect("TLS upload connection checked")
                            .begin_request(wire);
                        if let Err(error) = begun {
                            self.finish(slot, Completion::Failed(error), completions);
                            return Ok(queued);
                        }
                        queued |= self.pump_tls_request(slot, completions)?;
                    }
                    Ok(UploadWire::Pending | UploadWire::Complete) => return Ok(queued),
                    Err(error) => {
                        self.finish(slot, Completion::Failed(error), completions);
                        return Ok(queued);
                    }
                }
            } else {
                let capacity = match self.reactor.outbound_capacity(slot) {
                    Ok(capacity) => capacity,
                    Err(failure) => {
                        self.finish(
                            slot,
                            Completion::Failed(Error::new(ErrorKind::Internal, failure.message)),
                            completions,
                        );
                        return Ok(queued);
                    }
                };
                let next = self
                    .transfers
                    .get_mut(&slot)
                    .and_then(|transfer| transfer.upload.as_mut())
                    .expect("streamed upload presence checked")
                    .next_wire(Some(capacity));
                match next {
                    Ok(UploadWire::Bytes(wire)) => {
                        if let Err(failure) = self.reactor.queue_write(slot, &wire) {
                            self.finish(
                                slot,
                                Completion::Failed(Error::transport(
                                    TransportStage::Send,
                                    failure.message,
                                )),
                                completions,
                            );
                            return Ok(queued);
                        }
                        queued = true;
                    }
                    Ok(UploadWire::Pending | UploadWire::Complete) => return Ok(queued),
                    Err(error) => {
                        self.finish(slot, Completion::Failed(error), completions);
                        return Ok(queued);
                    }
                }
            }
        }
    }

    fn request_output_complete(transfer: &HttpTransfer) -> bool {
        if transfer.upload_aborted {
            return false;
        }
        let upload_complete = transfer
            .upload
            .as_ref()
            .is_none_or(NativeUpload::is_complete);
        let encrypted = transfer
            .tls
            .as_ref()
            .is_none_or(NativeTls::request_fully_encrypted);
        upload_complete && encrypted
    }

    fn refresh_request_write_drained(&mut self, slot: SlotId) -> Result<(), Error> {
        let complete = self
            .transfers
            .get(&slot)
            .is_some_and(|transfer| transfer.connected && Self::request_output_complete(transfer));
        if !complete {
            return Ok(());
        }
        if self
            .reactor
            .outbound_is_empty(slot)
            .map_err(native_internal_error)?
        {
            if let Some(transfer) = self.transfers.get_mut(&slot) {
                transfer.request_write_drained = true;
            }
        }
        Ok(())
    }

    fn handle_connected(
        &mut self,
        slot: SlotId,
        arm_deadline: bool,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<(), Error> {
        let start = self
            .transfers
            .get_mut(&slot)
            .and_then(|transfer| transfer.tls.as_mut())
            .map(NativeTls::start);
        match start {
            Some(Ok(outbound)) => {
                self.note_progress(slot, false, arm_deadline)?;
                if arm_deadline && !outbound.is_empty() {
                    if let Err(failure) = self.reactor.queue_write(slot, &outbound) {
                        self.finish(
                            slot,
                            Completion::Failed(Error::tls(
                                TransportStage::Tls,
                                TlsFailure::Io,
                                format!(
                                    "native TLS ClientHello queueing failed: {}",
                                    failure.message
                                ),
                            )),
                            completions,
                        );
                    }
                }
            }
            Some(Err(error)) => {
                self.finish(slot, Completion::Failed(error), completions);
            }
            None => self.note_progress(slot, true, arm_deadline)?,
        }
        Ok(())
    }

    fn consume_stream_plaintext(transfer: &mut HttpTransfer, consumed: usize) -> Result<(), Error> {
        match &mut transfer.response {
            TransferResponse::Streaming {
                retained_cleartext,
                retained_offset,
                ..
            } => {
                if let Some(tls) = transfer.tls.as_mut() {
                    tls.consume_retained_plaintext(consumed)
                } else {
                    let remaining = retained_cleartext.len().saturating_sub(*retained_offset);
                    if consumed > remaining {
                        return Err(Error::new(
                            ErrorKind::Internal,
                            "native HTTP consumed beyond retained cleartext streaming bytes",
                        ));
                    }
                    *retained_offset += consumed;
                    if *retained_offset == retained_cleartext.len() {
                        retained_cleartext.clear();
                        *retained_offset = 0;
                    }
                    Ok(())
                }
            }
            TransferResponse::Buffered(_) => Err(Error::new(
                ErrorKind::Internal,
                "buffered native response entered streaming plaintext consumption",
            )),
        }
    }

    fn stream_plaintext(transfer: &HttpTransfer) -> &[u8] {
        match &transfer.response {
            TransferResponse::Streaming {
                retained_cleartext,
                retained_offset,
                ..
            } => transfer.tls.as_ref().map_or_else(
                || &retained_cleartext[*retained_offset..],
                NativeTls::retained_plaintext,
            ),
            TransferResponse::Buffered(_) => &[],
        }
    }

    fn refresh_stream_allowance(&mut self, slot: SlotId) -> Result<(), Error> {
        let stream_state = self.transfers.get(&slot).and_then(|transfer| {
            let TransferResponse::Streaming {
                decoder,
                retained_cleartext,
                retained_offset,
                ..
            } = &transfer.response
            else {
                return None;
            };
            let retained = transfer.tls.as_ref().map_or_else(
                || retained_cleartext.len().saturating_sub(*retained_offset),
                |tls| tls.retained_plaintext().len(),
            );
            let capacity = decoder.socket_capacity();
            let allowance = if let Some(tls) = &transfer.tls {
                tls.streaming_read_allowance(capacity)
            } else if retained != 0 {
                0
            } else {
                capacity
            };
            Some((allowance, decoder.is_consumer_blocked()))
        });
        if let Some((allowance, consumer_blocked)) = stream_state {
            let deadline_update = self.transfers.get_mut(&slot).and_then(|transfer| {
                let mut changed = false;
                if consumer_blocked && !transfer.inactivity_paused {
                    transfer.inactivity_paused = true;
                    transfer.inactivity_deadline = None;
                    changed = true;
                } else if !consumer_blocked && transfer.inactivity_paused {
                    transfer.inactivity_paused = false;
                    transfer.inactivity_deadline = transfer
                        .inactivity_timeout
                        .and_then(|timeout| Instant::now().checked_add(timeout));
                    changed = true;
                }
                changed.then(|| transfer.next_deadline())
            });
            if let Some(deadline) = deadline_update {
                self.reactor
                    .set_deadline(slot, deadline)
                    .map_err(native_internal_error)?;
            }
            self.reactor
                .set_read_allowance(slot, Some(allowance))
                .map_err(native_internal_error)?;
        }
        Ok(())
    }

    fn complete_stream_response(
        &mut self,
        slot: SlotId,
        response_permits_reuse: bool,
        peer_closed: bool,
    ) {
        let Some(mut transfer) = self.transfers.remove(&slot) else {
            self.reactor.cancel(slot);
            return;
        };
        self.request_to_slot.remove(&transfer.request_id);
        let request_id = transfer.request_id;
        let total_deadline = transfer.total_deadline;
        let next_redirect_hops = transfer.redirect_hops.saturating_add(1);
        let mut redirect_transfer_failed = false;
        let redirect = match &mut transfer.response {
            TransferResponse::Streaming {
                decoder, redirect, ..
            } if redirect.is_some() => {
                let request = redirect.take().expect("redirect presence checked");
                match decoder.take_response() {
                    Ok(response) => Some((request, response)),
                    Err(error) => {
                        redirect_transfer_failed = true;
                        decoder.fail(error);
                        None
                    }
                }
            }
            TransferResponse::Streaming { .. } => None,
            TransferResponse::Buffered(_) => {
                debug_assert!(false, "buffered response entered streaming completion");
                None
            }
        };
        let per_origin_idle = self.idle.get(&transfer.key).map_or(0, VecDeque::len);
        let reusable = response_permits_reuse
            && !redirect_transfer_failed
            && transfer.request_permits_reuse
            && transfer.request_write_drained
            && !peer_closed
            && self.connection_limits.idle_timeout != Duration::ZERO
            && self.idle_count < self.connection_limits.idle_global
            && per_origin_idle < self.connection_limits.idle_per_origin;
        if reusable {
            let parked = Instant::now()
                .checked_add(self.connection_limits.idle_timeout)
                .filter(|idle_deadline| {
                    self.reactor
                        .set_deadline(slot, Some(*idle_deadline))
                        .is_ok()
                });
            if let Some(expires_at) = parked {
                self.idle_slots.insert(slot, transfer.key.clone());
                self.idle
                    .entry(transfer.key)
                    .or_default()
                    .push_back(IdleConnection {
                        slot,
                        tls: transfer.tls.take(),
                        expires_at,
                    });
                self.idle_count += 1;
                if let Some(metrics) = &self.metrics {
                    metrics.set_idle_connections(self.idle_count);
                }
            } else {
                self.reactor.cancel(slot);
                self.release_connection(&transfer.key);
            }
        } else {
            self.reactor.cancel(slot);
            self.release_connection(&transfer.key);
        }

        if let Some((request, response)) = redirect {
            let now = Instant::now();
            let deadlines = PendingDeadlines {
                connect: request
                    .options()
                    .connect_timeout
                    .and_then(|timeout| now.checked_add(timeout)),
                total: total_deadline,
                inactivity: request
                    .options()
                    .inactivity_timeout
                    .and_then(|timeout| now.checked_add(timeout)),
            };
            match self.make_pending(
                request_id,
                request,
                deadlines,
                next_redirect_hops,
                ErrorKind::Redirect,
                PendingResponse::Streaming {
                    response,
                    upload: None,
                },
            ) {
                Ok(pending) => {
                    let completion = self.start_pending(pending);
                    debug_assert!(completion.is_none());
                }
                Err((
                    error,
                    PendingResponse::Streaming {
                        mut response,
                        upload: _,
                    },
                )) => response.fail(error),
                Err((_error, PendingResponse::Buffered)) => {
                    unreachable!("stream redirect changed pending response kind")
                }
            }
        }
    }

    fn drain_stream_plaintext(
        &mut self,
        slot: SlotId,
        peer_closed: bool,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<(), Error> {
        if peer_closed {
            if let Some(HttpTransfer {
                response: TransferResponse::Streaming { peer_closed, .. },
                ..
            }) = self.transfers.get_mut(&slot)
            {
                *peer_closed = true;
            }
        }
        loop {
            let progress = {
                let Some(transfer) = self.transfers.get_mut(&slot) else {
                    return Ok(());
                };
                let HttpTransfer {
                    response,
                    response_started,
                    tls,
                    ..
                } = transfer;
                let TransferResponse::Streaming {
                    decoder,
                    retained_cleartext,
                    retained_offset,
                    ..
                } = response
                else {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "buffered native response entered streaming decoder drain",
                    ));
                };
                let plaintext = tls.as_ref().map_or_else(
                    || &retained_cleartext[*retained_offset..],
                    NativeTls::retained_plaintext,
                );
                if plaintext.is_empty() {
                    None
                } else {
                    *response_started = true;
                    Some(decoder.ingest(plaintext))
                }
            };

            let Some(progress) = progress else {
                break;
            };
            let progress = match progress {
                Ok(progress) => progress,
                Err(error) => {
                    self.finish(slot, Completion::Failed(error), completions);
                    return Ok(());
                }
            };
            match progress {
                StreamDecodeProgress::Head { head, consumed } => {
                    let redirect = self.transfers.get(&slot).map(|transfer| {
                        if transfer.upload.is_some() {
                            Ok(None)
                        } else {
                            redirected_request(
                                &transfer.request,
                                head.status(),
                                transfer.redirect_hops,
                                || {
                                    resolved_redirect_target_from_headers(
                                        &transfer.request,
                                        head.headers(),
                                    )
                                },
                            )
                        }
                    });
                    let (deliver, redirect) = match redirect {
                        Some(Ok(Some(request))) => (false, Some(request)),
                        Some(Ok(None)) => (true, None),
                        Some(Err(error)) => {
                            self.finish(slot, Completion::Failed(error), completions);
                            return Ok(());
                        }
                        None => return Ok(()),
                    };
                    let decision = if let Some(transfer) = self.transfers.get_mut(&slot) {
                        Self::consume_stream_plaintext(transfer, consumed)?;
                        if let Some(mut upload) = transfer.upload.take() {
                            transfer.upload_aborted |=
                                !upload.is_complete() || !transfer.request_write_drained;
                            upload.close();
                        }
                        let TransferResponse::Streaming {
                            decoder,
                            redirect: pending_redirect,
                            ..
                        } = &mut transfer.response
                        else {
                            unreachable!("streaming transfer changed response kind")
                        };
                        *pending_redirect = redirect;
                        decoder.decide_head(deliver)
                    } else {
                        return Ok(());
                    };
                    match decision {
                        Ok(StreamHeadDecision {
                            complete: true,
                            permits_reuse,
                        }) => {
                            let trailing = self.transfers.get(&slot).is_some_and(|transfer| {
                                !Self::stream_plaintext(transfer).is_empty()
                            });
                            let redirecting = self.transfers.get(&slot).is_some_and(|transfer| {
                                matches!(
                                    transfer.response,
                                    TransferResponse::Streaming {
                                        redirect: Some(_),
                                        ..
                                    }
                                )
                            });
                            if trailing && redirecting {
                                self.finish(
                                    slot,
                                    Completion::Failed(Error::transport(
                                        TransportStage::Http,
                                        "the peer sent bytes after a no-body redirect response",
                                    )),
                                    completions,
                                );
                                return Ok(());
                            }
                            let peer_closed = self.transfers.get(&slot).is_some_and(|transfer| {
                                matches!(
                                    transfer.response,
                                    TransferResponse::Streaming {
                                        peer_closed: true,
                                        ..
                                    }
                                )
                            });
                            self.complete_stream_response(
                                slot,
                                permits_reuse && !trailing,
                                peer_closed,
                            );
                            return Ok(());
                        }
                        Ok(StreamHeadDecision {
                            complete: false, ..
                        }) => {}
                        Err(error) => {
                            self.finish(slot, Completion::Failed(error), completions);
                            return Ok(());
                        }
                    }
                }
                StreamDecodeProgress::Body { consumed, blocked } => {
                    if let Some(transfer) = self.transfers.get_mut(&slot) {
                        Self::consume_stream_plaintext(transfer, consumed)?;
                    }
                    if blocked || consumed == 0 {
                        break;
                    }
                }
                StreamDecodeProgress::Complete {
                    consumed,
                    permits_reuse,
                    ..
                } => {
                    if let Some(transfer) = self.transfers.get_mut(&slot) {
                        Self::consume_stream_plaintext(transfer, consumed)?;
                    }
                    let trailing = self
                        .transfers
                        .get(&slot)
                        .is_some_and(|transfer| !Self::stream_plaintext(transfer).is_empty());
                    if trailing {
                        self.finish(
                            slot,
                            Completion::Failed(Error::transport(
                                TransportStage::Http,
                                "the peer sent bytes after the completed streaming HTTP response",
                            )),
                            completions,
                        );
                    } else {
                        let peer_closed = self.transfers.get(&slot).is_some_and(|transfer| {
                            matches!(
                                transfer.response,
                                TransferResponse::Streaming {
                                    peer_closed: true,
                                    ..
                                }
                            )
                        });
                        self.complete_stream_response(slot, permits_reuse, peer_closed);
                    }
                    return Ok(());
                }
            }
        }

        let should_eof = self.transfers.get(&slot).is_some_and(|transfer| {
            matches!(
                transfer.response,
                TransferResponse::Streaming {
                    peer_closed: true,
                    ..
                }
            ) && Self::stream_plaintext(transfer).is_empty()
        });
        if should_eof {
            let tls_dirty_eof = self.transfers.get(&slot).is_some_and(|transfer| {
                matches!(
                    transfer.response,
                    TransferResponse::Streaming {
                        tls_dirty_eof: true,
                        ..
                    }
                )
            });
            if tls_dirty_eof {
                self.finish(
                    slot,
                    Completion::Failed(Error::transport(
                        TransportStage::Receive,
                        "the TLS peer closed without an authenticated close notification",
                    )),
                    completions,
                );
                return Ok(());
            }
            let eof = self.transfers.get_mut(&slot).map(|transfer| {
                let TransferResponse::Streaming { decoder, .. } = &mut transfer.response else {
                    unreachable!("streaming transfer changed response kind")
                };
                decoder.eof()
            });
            match eof {
                Some(Ok(Some((permits_reuse, _)))) => {
                    self.complete_stream_response(slot, permits_reuse, true)
                }
                Some(Ok(None)) => self.finish(
                    slot,
                    Completion::Failed(Error::transport(
                        TransportStage::Receive,
                        "the peer closed without completing a streaming response",
                    )),
                    completions,
                ),
                Some(Err(error)) => self.finish(slot, Completion::Failed(error), completions),
                None => {}
            }
        }
        if self.transfers.contains_key(&slot) {
            self.refresh_stream_allowance(slot)?;
        }
        Ok(())
    }

    fn handle_stream_data(
        &mut self,
        slot: SlotId,
        bytes: Vec<u8>,
        arm_deadline: bool,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<(), Error> {
        let tls_progress = self
            .transfers
            .get_mut(&slot)
            .and_then(|transfer| transfer.tls.as_mut())
            .map(|tls| tls.receive_streaming(&bytes));
        let (established, outbound, peer_closed) = match tls_progress {
            Some(Ok(TlsStreamProgress {
                outbound,
                handshake_complete,
                peer_closed,
            })) => (handshake_complete, outbound, peer_closed),
            Some(Err(error)) => {
                self.finish(slot, Completion::Failed(error), completions);
                return Ok(());
            }
            None => {
                let Some(transfer) = self.transfers.get_mut(&slot) else {
                    return Ok(());
                };
                let TransferResponse::Streaming {
                    retained_cleartext,
                    retained_offset,
                    ..
                } = &mut transfer.response
                else {
                    return Err(Error::new(
                        ErrorKind::Internal,
                        "buffered native response entered streaming data path",
                    ));
                };
                if *retained_offset != retained_cleartext.len() {
                    self.finish(
                        slot,
                        Completion::Failed(Error::new(
                            ErrorKind::Internal,
                            "native cleartext accepted bytes while streaming plaintext remained",
                        )),
                        completions,
                    );
                    return Ok(());
                }
                *retained_cleartext = bytes;
                *retained_offset = 0;
                (false, Vec::new(), false)
            }
        };
        self.note_progress(slot, established, arm_deadline)?;
        if arm_deadline && !outbound.is_empty() {
            if let Err(failure) = self.reactor.queue_write(slot, &outbound) {
                self.finish(
                    slot,
                    Completion::Failed(Error::tls(
                        TransportStage::Tls,
                        TlsFailure::Io,
                        failure.message,
                    )),
                    completions,
                );
                return Ok(());
            }
        }
        if arm_deadline && established && self.transfers.contains_key(&slot) {
            self.pump_request_output(slot, completions)?;
        }
        self.drain_stream_plaintext(slot, peer_closed, completions)
    }

    fn resume_streams(&mut self, completions: &mut Vec<BackendCompletion>) -> Result<(), Error> {
        let slots = self
            .transfers
            .iter()
            .filter_map(|(slot, transfer)| transfer.response.is_streaming().then_some(*slot))
            .collect::<Vec<_>>();
        for slot in slots {
            self.pump_request_output(slot, completions)?;
            if self.transfers.contains_key(&slot) {
                self.refresh_request_write_drained(slot)?;
            }
            self.drain_stream_plaintext(slot, false, completions)?;
            if self.transfers.contains_key(&slot) {
                self.refresh_stream_allowance(slot)?;
            }
        }
        Ok(())
    }

    fn handle_data(
        &mut self,
        slot: SlotId,
        bytes: Vec<u8>,
        arm_deadline: bool,
        completions: &mut Vec<BackendCompletion>,
    ) -> Result<(), Error> {
        if self
            .transfers
            .get(&slot)
            .is_some_and(|transfer| transfer.response.is_streaming())
        {
            return self.handle_stream_data(slot, bytes, arm_deadline, completions);
        }
        let tls_progress = self
            .transfers
            .get_mut(&slot)
            .and_then(|transfer| transfer.tls.as_mut())
            .map(|tls| tls.receive(&bytes));
        let (plaintext, established, outbound, peer_closed) = match tls_progress {
            Some(Ok(TlsProgress {
                outbound,
                plaintext,
                handshake_complete,
                peer_closed,
            })) => (plaintext, handshake_complete, outbound, peer_closed),
            Some(Err(error)) => {
                self.finish(slot, Completion::Failed(error), completions);
                return Ok(());
            }
            None => (bytes, false, Vec::new(), false),
        };
        self.note_progress(slot, established, arm_deadline)?;
        if arm_deadline && !outbound.is_empty() {
            if let Err(failure) = self.reactor.queue_write(slot, &outbound) {
                let handshaking = self
                    .transfers
                    .get(&slot)
                    .and_then(|transfer| transfer.tls.as_ref())
                    .is_some_and(NativeTls::is_handshaking);
                let stage = if handshaking {
                    TransportStage::Tls
                } else {
                    TransportStage::Send
                };
                self.finish(
                    slot,
                    Completion::Failed(if stage == TransportStage::Tls {
                        Error::tls(stage, TlsFailure::Io, failure.message)
                    } else {
                        Error::transport(stage, failure.message)
                    }),
                    completions,
                );
                return Ok(());
            }
        }
        if arm_deadline && established && self.transfers.contains_key(&slot) {
            self.pump_request_output(slot, completions)?;
        }
        let decoded = self.transfers.get_mut(&slot).map(|transfer| {
            transfer.response_started |= !plaintext.is_empty();
            match &mut transfer.response {
                TransferResponse::Buffered(decoder) => decoder.ingest(&plaintext),
                TransferResponse::Streaming { .. } => unreachable!("streaming data branched above"),
            }
        });
        match decoded {
            Some(Ok(DecodeProgress {
                response: Some(response),
                consumed,
                permits_reuse,
            })) if consumed == plaintext.len() => {
                self.complete_response(slot, response, permits_reuse, peer_closed, completions)
            }
            Some(Ok(DecodeProgress {
                response: Some(_), ..
            })) => self.finish(
                slot,
                Completion::Failed(Error::transport(
                    TransportStage::Http,
                    "the peer sent bytes after the completed HTTP response",
                )),
                completions,
            ),
            Some(Err(error)) => self.finish(slot, Completion::Failed(error), completions),
            Some(Ok(DecodeProgress { response: None, .. })) if peer_closed => {
                let eof =
                    self.transfers
                        .get_mut(&slot)
                        .map(|transfer| match &mut transfer.response {
                            TransferResponse::Buffered(decoder) => decoder.eof(),
                            TransferResponse::Streaming { .. } => {
                                unreachable!("streaming data branched above")
                            }
                        });
                match eof {
                    Some(Ok(Some(response))) => {
                        self.finish(slot, Completion::Completed(response), completions)
                    }
                    Some(Err(error)) => self.finish(slot, Completion::Failed(error), completions),
                    Some(Ok(None)) => self.finish(
                        slot,
                        Completion::Failed(Error::transport(
                            TransportStage::Receive,
                            "the TLS peer closed without completing a response",
                        )),
                        completions,
                    ),
                    None => {}
                }
            }
            Some(Ok(DecodeProgress { response: None, .. })) | None => {}
        }
        Ok(())
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

    fn resume_standalone_tcp(&mut self) -> Result<(), Error> {
        let cancelled = self
            .standalone_pending
            .iter()
            .filter_map(|(slot, sink)| sink.is_terminal().then_some(*slot))
            .collect::<Vec<_>>();
        for slot in cancelled {
            if let Some(sink) = self.standalone_pending.remove(&slot) {
                self.standalone_request_to_slot.remove(&sink.id());
            }
            self.reactor.cancel(slot);
        }

        let slots = self.standalone_live.keys().copied().collect::<Vec<_>>();
        for slot in slots {
            let Some(mut live) = self.standalone_live.remove(&slot) else {
                continue;
            };
            if live.owner.session_released() {
                self.standalone_request_to_slot.remove(&live.request_id);
                self.reactor.cancel(slot);
                continue;
            }
            let result = (|| {
                live.sync_pressure(Instant::now());
                self.reactor
                    .set_read_allowance(slot, Some(live.owner.read_allowance()))
                    .map_err(native_internal_error)?;
                loop {
                    let capacity = self
                        .reactor
                        .outbound_capacity(slot)
                        .map_err(native_internal_error)?;
                    let Some(bytes) = live.owner.take_outbound_up_to(capacity) else {
                        break;
                    };
                    self.reactor
                        .queue_write(slot, &bytes)
                        .map_err(native_internal_error)?;
                }
                if live.owner.finish_requested()
                    && !live.owner.write_finished()
                    && live.owner.send_occupancy() == 0
                    && self
                        .reactor
                        .outbound_is_empty(slot)
                        .map_err(native_internal_error)?
                {
                    self.reactor
                        .shutdown_write(slot)
                        .map_err(native_transport_error)?;
                    live.owner.complete_write_shutdown()?;
                }
                live.sync_pressure(Instant::now());
                self.reactor
                    .set_deadline(slot, live.next_deadline())
                    .map_err(native_internal_error)?;
                Ok::<(), Error>(())
            })();
            if let Err(error) = result {
                live.owner.fail(error);
                self.standalone_request_to_slot.remove(&live.request_id);
                self.reactor.cancel(slot);
            } else {
                self.standalone_live.insert(slot, live);
            }
        }
        Ok(())
    }

    fn handle_standalone_event(
        &mut self,
        event: &NativeEvent,
        terminal_failure_in_batch: bool,
    ) -> Result<bool, Error> {
        let slot = match event {
            NativeEvent::Connected(slot)
            | NativeEvent::WriteProgress(slot, _)
            | NativeEvent::WriteDrained(slot)
            | NativeEvent::Data(slot, _)
            | NativeEvent::PeerClosed(slot)
            | NativeEvent::Failed(slot, _)
            | NativeEvent::DeadlineExpired(slot) => *slot,
        };
        if !self.standalone_pending.contains_key(&slot) && !self.standalone_live.contains_key(&slot)
        {
            return Ok(false);
        }

        match event {
            NativeEvent::Connected(slot) => {
                if terminal_failure_in_batch {
                    return Ok(true);
                }
                let Some(sink) = self.standalone_pending.remove(slot) else {
                    return Ok(true);
                };
                if sink.is_terminal() {
                    self.standalone_request_to_slot.remove(&sink.id());
                    self.reactor.cancel(*slot);
                    return Ok(true);
                }
                let local = self
                    .reactor
                    .local_addr(*slot)
                    .map_err(native_internal_error)?;
                let peer = self
                    .reactor
                    .peer_addr(*slot)
                    .map_err(native_internal_error)?;
                let Some(owner) = sink.connected(local, peer) else {
                    self.standalone_request_to_slot.remove(&sink.id());
                    self.reactor.cancel(*slot);
                    return Ok(true);
                };
                self.reactor
                    .set_deadline(*slot, None)
                    .map_err(native_internal_error)?;
                self.reactor
                    .set_read_allowance(*slot, Some(owner.read_allowance()))
                    .map_err(native_internal_error)?;
                let live = StandaloneTcp::new(
                    sink.id(),
                    owner,
                    sink.read_inactivity_timeout(),
                    sink.write_inactivity_timeout(),
                    Instant::now(),
                );
                self.reactor
                    .set_deadline(*slot, live.next_deadline())
                    .map_err(native_internal_error)?;
                self.standalone_live.insert(*slot, live);
            }
            NativeEvent::WriteProgress(slot, written) => {
                if let Some(live) = self.standalone_live.get_mut(slot) {
                    live.owner.write_progress(*written);
                    live.note_write_progress(Instant::now());
                    if !terminal_failure_in_batch {
                        self.reactor
                            .set_deadline(*slot, live.next_deadline())
                            .map_err(native_internal_error)?;
                    }
                }
            }
            NativeEvent::WriteDrained(slot) => {
                if let Some(live) = self.standalone_live.get_mut(slot) {
                    let progressed = live.owner.pump_bytes();
                    live.owner.write_progress(progressed);
                    live.note_write_progress(Instant::now());
                    if !terminal_failure_in_batch {
                        self.reactor
                            .set_deadline(*slot, live.next_deadline())
                            .map_err(native_internal_error)?;
                    }
                }
            }
            NativeEvent::Data(slot, bytes) => {
                if let Some(live) = self.standalone_live.get_mut(slot) {
                    live.note_read_progress(Instant::now());
                    if let Err(error) = live.owner.push_inbound(bytes.clone()) {
                        live.owner.fail(error);
                    } else {
                        live.sync_pressure(Instant::now());
                        if !terminal_failure_in_batch {
                            self.reactor
                                .set_deadline(*slot, live.next_deadline())
                                .map_err(native_internal_error)?;
                        }
                    }
                }
            }
            NativeEvent::PeerClosed(slot) => {
                if let Some(live) = self.standalone_live.get_mut(slot) {
                    live.owner.peer_closed();
                    live.note_peer_closed();
                    if !terminal_failure_in_batch {
                        self.reactor
                            .set_deadline(*slot, live.next_deadline())
                            .map_err(native_internal_error)?;
                    }
                }
            }
            NativeEvent::Failed(slot, failure) => {
                if let Some(pending) = self.standalone_pending.remove(slot) {
                    self.standalone_request_to_slot.remove(&pending.id());
                    if failure.kind == NativeFailureKind::Connect && !pending.remaining.is_empty() {
                        self.start_standalone_attempt(pending);
                    } else {
                        pending.sink.fail(native_transport_error(failure.clone()));
                    }
                }
                if let Some(mut live) = self.standalone_live.remove(slot) {
                    self.standalone_request_to_slot.remove(&live.request_id);
                    if failure.is_connection_reset() {
                        live.owner.reset();
                    } else {
                        live.owner.fail(native_transport_error(failure.clone()));
                    }
                }
            }
            NativeEvent::DeadlineExpired(slot) => {
                if terminal_failure_in_batch {
                    return Ok(true);
                }
                if let Some(pending) = self.standalone_pending.remove(slot) {
                    self.standalone_request_to_slot.remove(&pending.id());
                    pending.sink.fail(standalone_connect_timeout());
                    self.reactor.cancel(*slot);
                }
                if let Some(mut live) = self.standalone_live.remove(slot) {
                    if let Some(direction) = live.expired(Instant::now()) {
                        self.standalone_request_to_slot.remove(&live.request_id);
                        let message = match direction {
                            StandaloneInactivity::Read => {
                                "the standalone TCP read inactivity timeout expired"
                            }
                            StandaloneInactivity::Write => {
                                "the standalone TCP write inactivity timeout expired"
                            }
                        };
                        live.owner
                            .fail(Error::timeout(TimeoutKind::Inactivity, message));
                        self.reactor.cancel(*slot);
                    } else {
                        self.reactor
                            .set_deadline(*slot, live.next_deadline())
                            .map_err(native_internal_error)?;
                        self.standalone_live.insert(*slot, live);
                    }
                }
            }
        }
        Ok(true)
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
            let event_slot = match &event {
                NativeEvent::Connected(slot)
                | NativeEvent::WriteProgress(slot, _)
                | NativeEvent::WriteDrained(slot)
                | NativeEvent::Data(slot, _)
                | NativeEvent::PeerClosed(slot)
                | NativeEvent::Failed(slot, _)
                | NativeEvent::DeadlineExpired(slot) => *slot,
            };
            if self.handle_standalone_event(&event, failed_slots.contains(&event_slot))? {
                continue;
            }
            if self.idle_slots.contains_key(&event_slot) {
                match event {
                    NativeEvent::WriteProgress(_, _) | NativeEvent::WriteDrained(_) => {}
                    NativeEvent::Connected(_)
                    | NativeEvent::Data(_, _)
                    | NativeEvent::PeerClosed(_)
                    | NativeEvent::Failed(_, _)
                    | NativeEvent::DeadlineExpired(_) => self.discard_idle_slot(event_slot),
                }
                continue;
            }
            match event {
                NativeEvent::Connected(slot) => {
                    self.handle_connected(slot, !failed_slots.contains(&slot), &mut completions)?;
                    if !failed_slots.contains(&slot) && self.transfers.contains_key(&slot) {
                        self.pump_request_output(slot, &mut completions)?;
                    }
                }
                NativeEvent::WriteProgress(slot, _) => {
                    self.note_progress(slot, false, !failed_slots.contains(&slot))?;
                    if !failed_slots.contains(&slot) {
                        self.pump_request_output(slot, &mut completions)?;
                    }
                }
                NativeEvent::WriteDrained(slot) => {
                    self.note_progress(slot, false, !failed_slots.contains(&slot))?;
                    let queued_more = if failed_slots.contains(&slot) {
                        false
                    } else {
                        self.pump_request_output(slot, &mut completions)?
                    };
                    if !queued_more && self.transfers.contains_key(&slot) {
                        self.refresh_request_write_drained(slot)?;
                    }
                }
                NativeEvent::Data(slot, bytes) => {
                    self.handle_data(slot, bytes, !failed_slots.contains(&slot), &mut completions)?;
                }
                NativeEvent::PeerClosed(slot) => {
                    let tls_state = self
                        .transfers
                        .get(&slot)
                        .and_then(|transfer| transfer.tls.as_ref())
                        .map(|tls| tls.is_handshaking());
                    if tls_state == Some(true) {
                        self.finish(
                            slot,
                            Completion::Failed(Error::tls(
                                TransportStage::Tls,
                                TlsFailure::Protocol,
                                "the peer closed during the TLS handshake",
                            )),
                            &mut completions,
                        );
                        continue;
                    }
                    if tls_state == Some(false) {
                        if let Some(HttpTransfer {
                            response:
                                TransferResponse::Streaming {
                                    peer_closed,
                                    tls_dirty_eof,
                                    ..
                                },
                            ..
                        }) = self.transfers.get_mut(&slot)
                        {
                            *peer_closed = true;
                            *tls_dirty_eof = true;
                            self.drain_stream_plaintext(slot, false, &mut completions)?;
                            continue;
                        }
                        self.finish(
                            slot,
                            Completion::Failed(Error::transport(
                                TransportStage::Receive,
                                "the TLS peer closed without an authenticated close notification",
                            )),
                            &mut completions,
                        );
                        continue;
                    }
                    if self
                        .transfers
                        .get(&slot)
                        .is_some_and(|transfer| transfer.response.is_streaming())
                    {
                        self.drain_stream_plaintext(slot, true, &mut completions)?;
                        continue;
                    }
                    let decoded = self
                        .transfers
                        .get_mut(&slot)
                        .map(|transfer| match &mut transfer.response {
                            TransferResponse::Buffered(decoder) => decoder.eof(),
                            TransferResponse::Streaming { .. } => {
                                unreachable!("streaming peer close branched above")
                            }
                        });
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
    fn connection_metrics_available(&self) -> bool {
        true
    }

    fn attach_metrics(&mut self, metrics: Arc<Metrics>) {
        metrics.set_active_connections(self.connection_count);
        metrics.set_idle_connections(self.idle_count);
        metrics.set_connection_waiters(self.waiting.len());
        self.metrics = Some(metrics);
    }

    fn submit(
        &mut self,
        id: RequestId,
        request: Request,
        accepted_at: Instant,
    ) -> Option<Completion> {
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
        let pending = match self.make_pending(
            id,
            request,
            PendingDeadlines {
                connect: connect_deadline,
                total: total_deadline,
                inactivity: inactivity_deadline,
            },
            0,
            ErrorKind::InvalidRequest,
            PendingResponse::Buffered,
        ) {
            Ok(pending) => pending,
            Err((error, _response)) => return Some(Completion::Failed(error)),
        };
        self.start_pending(pending)
    }

    fn submit_stream(
        &mut self,
        id: RequestId,
        request: StreamRequest,
        response: ResponseSink,
        accepted_at: Instant,
    ) {
        let (request, upload) = request.into_parts();
        let options = request.options();
        let deadlines = PendingDeadlines {
            connect: options
                .connect_timeout
                .and_then(|timeout| accepted_at.checked_add(timeout)),
            total: options
                .total_timeout
                .and_then(|timeout| accepted_at.checked_add(timeout)),
            inactivity: options
                .inactivity_timeout
                .and_then(|timeout| accepted_at.checked_add(timeout)),
        };
        let pending = match self.make_pending(
            id,
            request,
            deadlines,
            0,
            ErrorKind::InvalidRequest,
            PendingResponse::Streaming { response, upload },
        ) {
            Ok(pending) => pending,
            Err((
                error,
                PendingResponse::Streaming {
                    mut response,
                    upload: _,
                },
            )) => {
                response.fail(error);
                return;
            }
            Err((_error, PendingResponse::Buffered)) => {
                unreachable!("stream submission changed pending response kind")
            }
        };
        let completion = self.start_pending(pending);
        debug_assert!(
            completion.is_none(),
            "streaming pending failures must commit directly into ResponseSink"
        );
    }

    fn cancel(&mut self, id: RequestId) {
        if let Some(key) = self.standalone_request_to_resolve.remove(&id) {
            self.standalone_resolves.remove(&key);
            if let Some(resolver) = &self.resolver {
                let _cancel_result = resolver.cancel(key);
            }
            return;
        }
        if let Some(slot) = self.standalone_request_to_slot.remove(&id) {
            self.standalone_pending.remove(&slot);
            if let Some(mut live) = self.standalone_live.remove(&slot) {
                live.owner.cancel();
            }
            self.reactor.cancel(slot);
            return;
        }
        if let Some(index) = self
            .waiting
            .iter()
            .position(|pending| pending.request_id == id)
        {
            self.waiting.remove(index);
            if let Some(metrics) = &self.metrics {
                metrics.set_connection_waiters(self.waiting.len());
            }
        }
        if let Some(key) = self.request_to_resolve.remove(&id) {
            if let Some(pending) = self.resolves.remove(&key) {
                self.release_connection(&pending.key);
            }
            if let Some(resolver) = &self.resolver {
                let _cancel_result = resolver.cancel(key);
            }
        }
        if let Some(key) = self.request_to_public.remove(&id) {
            self.public_lookups.remove(&key);
            if let Some(resolver) = &self.resolver {
                let _cancel_result = resolver.cancel(key);
            }
        }
        if let Some(slot) = self.request_to_slot.remove(&id) {
            if let Some(transfer) = self.transfers.remove(&slot) {
                self.release_connection(&transfer.key);
            }
            self.reactor.cancel(slot);
        }
    }

    fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
        self.resume_standalone_tcp()?;
        self.expire_idle(Instant::now());
        let mut completions = self.dispatch_waiting();
        self.drain_dns()?;
        completions.extend(std::mem::take(&mut self.pending_http_from_dns));
        completions.extend(self.expire_resolves()?);
        completions.extend(self.dispatch_waiting());
        self.resume_streams(&mut completions)?;
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
        self.resume_standalone_tcp()?;
        self.drain_dns()?;
        completions.extend(std::mem::take(&mut self.pending_http_from_dns));
        completions.extend(self.expire_resolves()?);
        completions.extend(self.dispatch_waiting());
        Ok(completions)
    }

    fn shutdown(&mut self) -> Result<(), ShutdownError> {
        let closing = self.connection_count;
        self.request_to_slot.clear();
        self.transfers.clear();
        self.request_to_resolve.clear();
        self.resolves.clear();
        self.request_to_public.clear();
        self.public_lookups.clear();
        self.pending_public.clear();
        self.standalone_request_to_resolve.clear();
        self.standalone_resolves.clear();
        self.pending_http_from_dns.clear();
        self.idle.clear();
        self.idle_slots.clear();
        self.idle_count = 0;
        self.connection_count = 0;
        self.connections_per_key.clear();
        self.waiting.clear();
        self.standalone_request_to_slot.clear();
        self.standalone_pending.clear();
        self.standalone_live.clear();
        if let Some(metrics) = &self.metrics {
            for _ in 0..closing {
                metrics.connection_closed(0);
            }
            metrics.set_idle_connections(0);
            metrics.set_connection_waiters(0);
        }
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

    fn wants_poll_without_requests(&self) -> bool {
        self.idle_count != 0
            || !self.public_lookups.is_empty()
            || !self.standalone_resolves.is_empty()
            || !self.standalone_pending.is_empty()
            || !self.standalone_live.is_empty()
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_public_resolver(&self) -> bool {
        self.resolver.is_some()
    }

    fn supports_standalone_tcp(&self) -> bool {
        true
    }

    fn submit_tcp_connect(
        &mut self,
        request: TcpConnectRequest,
        sink: TcpConnectSink,
        accepted_at: Instant,
    ) {
        let deadline = request
            .connect_timeout()
            .and_then(|timeout| accepted_at.checked_add(timeout));
        match request.target() {
            TcpConnectTarget::Literal(address) => {
                self.start_standalone_attempt(StandalonePending {
                    sink,
                    remaining: VecDeque::from([*address]),
                    connect_deadline: deadline,
                });
            }
            TcpConnectTarget::Hostname { name, port } => {
                if self.resolver.is_none() {
                    sink.fail(Error::new(
                        ErrorKind::Unsupported,
                        "hostname TCP connections require the native resolver owner",
                    ));
                    return;
                }
                if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                    sink.fail(standalone_connect_timeout());
                    return;
                }
                let key = match self.next_resolve_key() {
                    Ok(key) => key,
                    Err(error) => {
                        sink.fail(error);
                        return;
                    }
                };
                if let Err(error) = self
                    .resolver
                    .as_ref()
                    .expect("resolver presence checked")
                    .public_resolve(
                        key,
                        PublicResolveSpec {
                            host: name.clone(),
                            family: AddressFamily::Both,
                            order: AddressOrder::Ipv4ThenIpv6,
                            cache_mode: CacheMode::Use,
                            max_results: sink.max_resolve_results(),
                            expand_search: false,
                        },
                    )
                {
                    sink.fail(error);
                    return;
                }
                self.standalone_request_to_resolve.insert(sink.id(), key);
                self.standalone_resolves.insert(
                    key,
                    StandaloneResolve {
                        sink,
                        port: *port,
                        connect_deadline: deadline,
                    },
                );
            }
        }
    }

    fn submit_resolve(
        &mut self,
        id: RequestId,
        request: ResolveRequest,
        accepted_at: Instant,
        max_results: usize,
    ) -> Option<ResolveCompletion> {
        if self.resolver.is_none() {
            return Some(ResolveCompletion::Failed(Error::new(
                ErrorKind::Unsupported,
                "public hostname resolution is not available on this Engine",
            )));
        }
        if request
            .total_timeout()
            .and_then(|timeout| accepted_at.checked_add(timeout))
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            return Some(public_total_timeout());
        }
        let key = match self.next_resolve_key() {
            Ok(key) => key,
            Err(error) => return Some(ResolveCompletion::Failed(error)),
        };
        if let Err(error) = self
            .resolver
            .as_ref()
            .expect("resolver presence checked")
            .public_resolve(
                key,
                PublicResolveSpec {
                    host: request.name().to_owned(),
                    family: request.address_family(),
                    order: request.address_order(),
                    cache_mode: request.cache_mode(),
                    max_results,
                    expand_search: request.applies_search_suffixes(),
                },
            )
        {
            return Some(ResolveCompletion::Failed(error));
        }
        self.request_to_public.insert(id, key);
        self.public_lookups.insert(
            key,
            PublicLookup {
                request_id: id,
                total_deadline: request
                    .total_timeout()
                    .and_then(|timeout| accepted_at.checked_add(timeout)),
            },
        );
        None
    }

    fn poll_resolves(&mut self) -> Result<Vec<BackendResolveCompletion>, Error> {
        self.drain_dns()?;
        Ok(std::mem::take(&mut self.pending_public))
    }
}

fn public_total_timeout() -> ResolveCompletion {
    ResolveCompletion::Failed(Error::timeout(
        TimeoutKind::Total,
        "the public resolution total timeout expired",
    ))
}

fn standalone_connect_timeout() -> Error {
    Error::timeout(
        TimeoutKind::Connect,
        "the standalone TCP connection-establishment timeout expired",
    )
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

#[cfg(any(test, fuzzing))]
#[derive(Debug, Eq, PartialEq)]
enum FuzzDecodeOutcome {
    Complete {
        response: Response,
        consumed: usize,
        permits_reuse: bool,
    },
    Failed(Error),
    Eof {
        response: Option<Response>,
        permits_reuse: bool,
    },
}

#[cfg(any(test, fuzzing))]
fn fuzz_decode_with<F>(
    wire: &[u8],
    response_to_head: bool,
    limits: HttpLimits,
    mut next_chunk: F,
) -> FuzzDecodeOutcome
where
    F: FnMut(usize) -> usize,
{
    let mut decoder = ResponseDecoder::new(response_to_head, limits);
    let mut offset = 0;
    while offset < wire.len() {
        let remaining = wire.len() - offset;
        let chunk_len = next_chunk(remaining).clamp(1, remaining);
        match decoder.ingest(&wire[offset..offset + chunk_len]) {
            Ok(progress) => {
                if let Some(response) = progress.response {
                    return FuzzDecodeOutcome::Complete {
                        response,
                        consumed: offset + progress.consumed,
                        permits_reuse: progress.permits_reuse,
                    };
                }
                offset += progress.consumed;
            }
            Err(error) => return FuzzDecodeOutcome::Failed(error),
        }
    }
    let permits_reuse = decoder.permits_reuse;
    match decoder.eof() {
        Ok(response) => FuzzDecodeOutcome::Eof {
            response,
            permits_reuse,
        },
        Err(error) => FuzzDecodeOutcome::Failed(error),
    }
}

/// Coverage-guided entry point used by `fuzz/fuzz_targets/native_response_decoder.rs`.
///
/// The first six bytes select bounded parser limits and an irregular fragmentation schedule. The
/// remainder is the HTTP wire image. Comparing three schedules turns arbitrary mutation into a
/// state-machine oracle instead of merely checking that the parser does not panic.
#[cfg(any(test, fuzzing))]
pub(crate) fn fuzz_response_decoder(data: &[u8]) {
    const CONTROL_BYTES: usize = 6;
    if data.len() < CONTROL_BYTES {
        return;
    }
    let control = &data[..CONTROL_BYTES];
    let encoded_wire = &data[CONTROL_BYTES..];
    let decoded_wire;
    let wire = if control[0] & 2 != 0 {
        decoded_wire = decode_fuzz_seed_escapes(encoded_wire);
        decoded_wire.as_slice()
    } else {
        encoded_wire
    };
    let response_to_head = control[0] & 1 != 0;
    let limits = HttpLimits {
        body_bytes: usize::from(control[1]).saturating_mul(64),
        header_bytes: usize::from(control[2]).saturating_mul(16).max(1),
        header_count: usize::from(control[3] % 64).saturating_add(1),
    };

    let whole = fuzz_decode_with(wire, response_to_head, limits, |remaining| remaining);
    let bytewise = fuzz_decode_with(wire, response_to_head, limits, |_| 1);
    assert_eq!(
        whole, bytewise,
        "native response decoding changed under bytewise fragmentation"
    );

    let mut turn = 0_usize;
    let irregular = fuzz_decode_with(wire, response_to_head, limits, |remaining| {
        let selector = control[4 + (turn & 1)];
        turn = turn.wrapping_add(1);
        usize::from(selector % 64).saturating_add(1).min(remaining)
    });
    assert_eq!(
        whole, irregular,
        "native response decoding changed under irregular fragmentation"
    );
}

#[cfg(any(test, fuzzing))]
fn decode_fuzz_seed_escapes(bytes: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            match bytes[index + 1] {
                b'r' => {
                    decoded.push(b'\r');
                    index += 2;
                    continue;
                }
                b'n' => {
                    decoded.push(b'\n');
                    index += 2;
                    continue;
                }
                b'\\' => {
                    decoded.push(b'\\');
                    index += 2;
                    continue;
                }
                _ => {}
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    decoded
}

/// Coverage-guided entry point for the streaming decoder and its reader backpressure boundary.
#[cfg(any(test, fuzzing))]
pub(crate) fn fuzz_streaming_response_decoder(data: &[u8], handle: crate::RequestHandle) {
    const CONTROL_BYTES: usize = 8;
    if data.len() < CONTROL_BYTES {
        return;
    }
    let control = &data[..CONTROL_BYTES];
    let encoded_wire = &data[CONTROL_BYTES..];
    let decoded_wire;
    let wire = if control[0] & 2 != 0 {
        decoded_wire = decode_fuzz_seed_escapes(encoded_wire);
        decoded_wire.as_slice()
    } else {
        encoded_wire
    };
    let deliver = control[0] & 4 != 0;
    let limits = HttpLimits {
        body_bytes: usize::from(control[1]).saturating_mul(64),
        header_bytes: usize::from(control[2]).saturating_mul(16).max(1),
        header_count: usize::from(control[3] % 64).saturating_add(1),
    };
    let queue_capacity = usize::from(control[4] % 64).saturating_add(1);
    let (mut reader, sink, _control) = crate::stream::response_pair(
        handle,
        crate::RunMode::Manual,
        queue_capacity,
        limits.body_bytes,
        None,
        None,
    )
    .expect("streaming decoder fuzz response pair must construct");
    let mut decoder = StreamingResponseDecoder::new(control[0] & 1 != 0, limits, sink);
    let mut offset = 0_usize;
    let mut turn = 0_usize;

    while offset < wire.len() {
        let remaining = wire.len() - offset;
        let selector = control[5 + (turn & 1)];
        turn = turn.wrapping_add(1);
        let chunk_len = usize::from(selector % 64).saturating_add(1).min(remaining);
        let progress = match decoder.ingest(&wire[offset..offset + chunk_len]) {
            Ok(progress) => progress,
            Err(error) => {
                decoder.fail(error);
                return;
            }
        };
        match progress {
            StreamDecodeProgress::Head { consumed, .. } => {
                assert!(consumed != 0 && consumed <= chunk_len);
                offset += consumed;
                let decision = match decoder.decide_head(deliver) {
                    Ok(decision) => decision,
                    Err(error) => {
                        decoder.fail(error);
                        return;
                    }
                };
                if decision.complete {
                    finish_fuzz_stream_decoder(decoder, deliver);
                    return;
                }
            }
            StreamDecodeProgress::Body { consumed, blocked } => {
                assert!(consumed <= chunk_len);
                offset += consumed;
                if blocked {
                    assert!(deliver, "discarded streaming response became backpressured");
                    let mut destination = vec![
                        0_u8;
                        usize::from(control[7] % 64)
                            .saturating_add(1)
                            .min(queue_capacity)
                    ];
                    match reader.try_read(&mut destination) {
                        Ok(crate::StreamRead::Data(read)) => {
                            assert!(read != 0, "backpressured reader released no capacity");
                        }
                        Ok(crate::StreamRead::Pending) => {
                            panic!("backpressured decoder had no reader-visible bytes")
                        }
                        Ok(crate::StreamRead::Eof) => {
                            panic!("backpressured decoder reached reader EOF")
                        }
                        Err(_) => return,
                    }
                } else {
                    assert!(consumed != 0, "streaming decoder made no progress");
                }
            }
            StreamDecodeProgress::Complete {
                consumed,
                delivered,
                ..
            } => {
                assert!(consumed != 0 && consumed <= chunk_len);
                assert_eq!(delivered, deliver);
                finish_fuzz_stream_decoder(decoder, deliver);
                return;
            }
        }
    }

    match decoder.eof() {
        Ok(Some((_permits_reuse, delivered))) => {
            assert_eq!(delivered, deliver);
            finish_fuzz_stream_decoder(decoder, deliver);
        }
        Ok(None) => {}
        Err(error) => decoder.fail(error),
    }
}

#[cfg(any(test, fuzzing))]
fn finish_fuzz_stream_decoder(decoder: StreamingResponseDecoder, delivered: bool) {
    if !delivered {
        let mut sink = decoder
            .into_response()
            .expect("discarded complete response must return its sink");
        sink.cancel();
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
        TransportStage, TryPushErrorKind, UploadBody,
    };

    const LIMITS: HttpLimits = HttpLimits {
        body_bytes: 1024,
        header_bytes: 1024,
        header_count: 16,
    };

    #[test]
    fn checked_in_fuzz_seeds_satisfy_the_fragmentation_oracle() {
        for seed in [
            include_bytes!("../../fuzz/corpus/native_response_decoder/fixed.seed").as_slice(),
            include_bytes!("../../fuzz/corpus/native_response_decoder/chunked.seed").as_slice(),
            include_bytes!("../../fuzz/corpus/native_response_decoder/informational.seed")
                .as_slice(),
            include_bytes!("../../fuzz/corpus/native_response_decoder/close_delimited.seed")
                .as_slice(),
            include_bytes!("../../fuzz/corpus/native_response_decoder/conflicting_length.seed")
                .as_slice(),
            include_bytes!("../../fuzz/corpus/native_response_decoder/malformed_chunk.seed")
                .as_slice(),
        ] {
            fuzz_response_decoder(seed);
        }
    }

    #[test]
    fn checked_in_streaming_fuzz_seeds_cross_reader_backpressure() {
        for seed in [
            include_bytes!(
                "../../fuzz/corpus/native_streaming_response_decoder/fixed_tiny_queue.seed"
            )
            .as_slice(),
            include_bytes!(
                "../../fuzz/corpus/native_streaming_response_decoder/chunked_tiny_queue.seed"
            )
            .as_slice(),
            include_bytes!("../../fuzz/corpus/native_streaming_response_decoder/no_body.seed")
                .as_slice(),
            include_bytes!(
                "../../fuzz/corpus/native_streaming_response_decoder/close_delimited.seed"
            )
            .as_slice(),
            include_bytes!(
                "../../fuzz/corpus/native_streaming_response_decoder/discard_redirect.seed"
            )
            .as_slice(),
        ] {
            let (engine, _controller) = crate::testing::engine(EngineConfig::manual())
                .expect("streaming fuzz seed Engine must construct");
            let pending = engine
                .client()
                .submit(
                    Request::get("http://fuzz.invalid/")
                        .build()
                        .expect("streaming fuzz seed request must build"),
                )
                .expect("streaming fuzz seed request must submit");
            let handle = pending.handle();
            drop(pending);
            fuzz_streaming_response_decoder(seed, handle);
            engine.cancel_all();
            engine
                .shutdown()
                .expect("streaming fuzz seed Engine must stop");
        }
    }

    fn synthetic_stream_decoder(
        queue_capacity: usize,
    ) -> (Engine, crate::ResponseReader, StreamingResponseDecoder) {
        let (engine, _controller) =
            crate::testing::engine(EngineConfig::spawned()).expect("held Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get("http://example.test/")
                    .build()
                    .expect("synthetic handle request must build"),
            )
            .expect("synthetic handle request must submit");
        let handle = pending.handle();
        drop(pending);
        let (reader, sink, _control) = crate::stream::response_pair(
            handle,
            crate::RunMode::Spawned,
            queue_capacity,
            LIMITS.body_bytes,
            None,
            None,
        )
        .expect("synthetic response pair must construct");
        (
            engine,
            reader,
            StreamingResponseDecoder::new(false, LIMITS, sink),
        )
    }

    #[test]
    fn streaming_decoder_publishes_head_then_body_and_reports_exact_boundary() {
        let (engine, reader, mut decoder) = synthetic_stream_decoder(8);
        let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Test: yes\r\n\r\nhelloextra";
        let head_consumed = match decoder.ingest(wire).expect("head must parse") {
            StreamDecodeProgress::Head { head, consumed } => {
                assert_eq!(head.status(), 200);
                assert!(head.headers().iter().any(|header| {
                    header.name().eq_ignore_ascii_case("x-test") && header.value() == b"yes"
                }));
                consumed
            }
            other => panic!("stream decoder skipped the head boundary: {other:?}"),
        };
        let decision = decoder.decide_head(true).expect("head must publish");
        assert!(!decision.complete);
        let body_consumed = match decoder
            .ingest(&wire[head_consumed..])
            .expect("body must decode")
        {
            StreamDecodeProgress::Complete {
                consumed,
                delivered,
                ..
            } => {
                assert!(delivered);
                consumed
            }
            other => panic!("stream body did not complete: {other:?}"),
        };
        assert_eq!(body_consumed, 5, "trailing bytes must remain unconsumed");
        let response = reader
            .collect()
            .expect("reader must collect delivered body");
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"hello");
        engine.cancel_all();
        engine.shutdown().expect("held Engine must stop");
    }

    #[test]
    fn streaming_decoder_splits_body_to_the_current_reader_hole() {
        let (engine, mut reader, mut decoder) = synthetic_stream_decoder(3);
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n";
        assert!(matches!(
            decoder.ingest(head).expect("head must parse"),
            StreamDecodeProgress::Head { consumed, .. } if consumed == head.len()
        ));
        decoder.decide_head(true).expect("head must publish");

        let body = b"abcdefgh";
        let mut offset = 0;
        let mut received = Vec::new();
        while offset < body.len() {
            match decoder
                .ingest(&body[offset..])
                .expect("bounded body pass must decode")
            {
                StreamDecodeProgress::Body { consumed, blocked } => {
                    assert!(blocked);
                    assert_ne!(consumed, 0);
                    offset += consumed;
                }
                StreamDecodeProgress::Complete { consumed, .. } => {
                    offset += consumed;
                }
                StreamDecodeProgress::Head { .. } => panic!("head was published twice"),
            }
            let mut hole = [0_u8; 2];
            if let Some(count) = reader
                .read(&mut hole)
                .expect("reader must drain a small hole")
            {
                received.extend_from_slice(&hole[..count]);
            }
        }
        let mut tail = [0_u8; 8];
        while let Some(count) = reader.read(&mut tail).expect("reader must reach EOF") {
            received.extend_from_slice(&tail[..count]);
        }
        assert_eq!(received, body);
        engine.cancel_all();
        engine.shutdown().expect("held Engine must stop");
    }

    #[test]
    fn streaming_decoder_no_body_head_is_complete_and_redirect_discard_reuses_sink() {
        let (engine, mut reader, mut decoder) = synthetic_stream_decoder(8);
        let no_body = b"HTTP/1.1 204 No Content\r\n\r\n";
        assert!(matches!(
            decoder.ingest(no_body).expect("204 head must parse"),
            StreamDecodeProgress::Head { .. }
        ));
        let decision = decoder.decide_head(true).expect("204 head must publish");
        assert!(decision.complete);
        assert_eq!(
            reader.wait_head().expect("204 head must arrive").status(),
            204
        );
        assert!(reader.is_eof());
        drop(reader);
        engine.cancel_all();
        engine.shutdown().expect("held Engine must stop");

        let (engine, reader, mut redirect) = synthetic_stream_decoder(8);
        let first = b"HTTP/1.1 302 Found\r\nContent-Length: 3\r\nLocation: /next\r\n\r\nold";
        let head_used = match redirect.ingest(first).expect("redirect head must parse") {
            StreamDecodeProgress::Head { consumed, .. } => consumed,
            other => panic!("redirect head was not exposed for policy: {other:?}"),
        };
        redirect
            .decide_head(false)
            .expect("redirect head must remain private");
        assert!(matches!(
            redirect
                .ingest(&first[head_used..])
                .expect("redirect body must drain"),
            StreamDecodeProgress::Complete {
                delivered: false,
                ..
            }
        ));
        let sink = redirect
            .into_response()
            .expect("discarded redirect must return the unique sink");
        let mut final_decoder = StreamingResponseDecoder::new(false, LIMITS, sink);
        let final_wire = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew";
        let head_used = match final_decoder
            .ingest(final_wire)
            .expect("final head must parse")
        {
            StreamDecodeProgress::Head { consumed, .. } => consumed,
            other => panic!("final head was not exposed: {other:?}"),
        };
        final_decoder
            .decide_head(true)
            .expect("final head must publish");
        assert!(matches!(
            final_decoder
                .ingest(&final_wire[head_used..])
                .expect("final body must decode"),
            StreamDecodeProgress::Complete {
                delivered: true,
                ..
            }
        ));
        let response = reader.collect().expect("reader sees only the final hop");
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"new");
        engine.cancel_all();
        engine.shutdown().expect("held Engine must stop");
    }

    #[test]
    fn streaming_close_delimited_body_drains_retained_bytes_before_fin_completes_it() {
        let (engine, mut reader, mut decoder) = synthetic_stream_decoder(3);
        let head = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
        assert!(matches!(
            decoder.ingest(head).expect("close-delimited head must parse"),
            StreamDecodeProgress::Head { consumed, .. } if consumed == head.len()
        ));
        decoder
            .decide_head(true)
            .expect("close-delimited head must publish");

        let body = b"abcde";
        let consumed = match decoder
            .ingest(body)
            .expect("first close-delimited window must decode")
        {
            StreamDecodeProgress::Body { consumed, blocked } => {
                assert!(blocked);
                consumed
            }
            other => panic!("close-delimited body did not pause: {other:?}"),
        };
        assert_eq!(consumed, 3);
        let mut first = [0_u8; 3];
        assert_eq!(reader.read(&mut first).expect("reader must drain"), Some(3));
        assert_eq!(&first, b"abc");

        assert!(matches!(
            decoder
                .ingest(&body[consumed..])
                .expect("retained close-delimited bytes must drain"),
            StreamDecodeProgress::Body {
                consumed: 2,
                blocked: false
            }
        ));
        assert_eq!(
            decoder.eof().expect("peer FIN must complete body"),
            Some((false, true))
        );
        let mut tail = [0_u8; 3];
        assert_eq!(reader.read(&mut tail).expect("tail must drain"), Some(2));
        assert_eq!(&tail[..2], b"de");
        assert_eq!(reader.read(&mut tail).expect("reader must reach EOF"), None);
        engine.cancel_all();
        engine.shutdown().expect("held Engine must stop");
    }

    #[test]
    fn streaming_decoder_preserves_informational_chunked_trailer_and_limit_rules() {
        let (engine, mut reader, mut decoder) = synthetic_stream_decoder(4);
        let wire = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nX-Trailer: yes\r\n\r\n";
        let head_used = match decoder.ingest(wire).expect("final head must parse") {
            StreamDecodeProgress::Head { head, consumed } => {
                assert_eq!(head.status(), 200);
                consumed
            }
            other => panic!("informational head escaped or final head vanished: {other:?}"),
        };
        decoder.decide_head(true).expect("final head must publish");
        let mut offset = head_used;
        let mut received = Vec::new();
        while offset < wire.len() {
            match decoder
                .ingest(&wire[offset..])
                .expect("chunked framing must decode")
            {
                StreamDecodeProgress::Body { consumed, blocked } => {
                    assert!(blocked || offset + consumed == wire.len());
                    assert_ne!(consumed, 0);
                    offset += consumed;
                }
                StreamDecodeProgress::Complete { consumed, .. } => {
                    offset += consumed;
                    break;
                }
                StreamDecodeProgress::Head { .. } => panic!("final head was published twice"),
            }
            let mut hole = [0_u8; 3];
            if let Some(count) = reader.read(&mut hole).expect("chunked reader must drain") {
                received.extend_from_slice(&hole[..count]);
            }
        }
        assert_eq!(offset, wire.len());
        let mut tail = [0_u8; 8];
        while let Some(count) = reader.read(&mut tail).expect("chunked reader must finish") {
            received.extend_from_slice(&tail[..count]);
        }
        assert_eq!(received, b"hello");
        engine.cancel_all();
        engine.shutdown().expect("held Engine must stop");

        let (engine, mut reader, mut decoder) = synthetic_stream_decoder(8);
        decoder.limits.body_bytes = 4;
        let error = decoder
            .ingest(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")
            .expect_err("oversize final head must fail before publication");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
        decoder.fail(error.clone());
        assert!(matches!(
            reader.try_head(),
            Err(crate::StreamError::Failed(observed)) if observed == error
        ));
        engine.cancel_all();
        engine.shutdown().expect("held Engine must stop");
    }

    fn read_request_head(stream: &mut std::net::TcpStream, label: &str) {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 512];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream
                .read(&mut buffer)
                .unwrap_or_else(|error| panic!("{label} must read: {error}"));
            assert_ne!(read, 0, "client closed before {label}");
            request.extend_from_slice(&buffer[..read]);
        }
    }

    fn read_request_wire(stream: &mut std::net::TcpStream, label: &str) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 512];
        let head_end = loop {
            if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
            let read = stream
                .read(&mut buffer)
                .unwrap_or_else(|error| panic!("{label} must read: {error}"));
            assert_ne!(read, 0, "client closed before {label}");
            request.extend_from_slice(&buffer[..read]);
        };
        let content_length = std::str::from_utf8(&request[..head_end])
            .expect("test request head must be UTF-8")
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("test Content-Length"))
                })
            })
            .unwrap_or(0);
        let total = head_end + content_length;
        while request.len() < total {
            let read = stream
                .read(&mut buffer)
                .unwrap_or_else(|error| panic!("{label} body must read: {error}"));
            assert_ne!(read, 0, "client closed before {label} body");
            request.extend_from_slice(&buffer[..read]);
        }
        request.truncate(total);
        request
    }

    fn push_eventually(sender: &mut crate::UploadSender, mut chunk: Vec<u8>, label: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match sender.try_push(chunk) {
                Ok(()) => return,
                Err(error) if error.kind() == TryPushErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "{label} remained backpressured");
                    chunk = error.into_chunk();
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("{label} failed unexpectedly: {error}"),
            }
        }
    }

    fn peer_observed_close(stream: &mut std::net::TcpStream) -> bool {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("peer close timeout must configure");
        let mut buffer = [0_u8; 64];
        match stream.read(&mut buffer) {
            Ok(0) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) =>
            {
                true
            }
            Ok(_) | Err(_) => false,
        }
    }

    #[test]
    fn terminal_socket_failure_dominates_same_batch_write_progress() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("batch fixture must bind");
        let address = listener.local_addr().expect("batch fixture address");
        let config = EngineConfig::manual();
        let mut backend =
            NativeHttpBackend::new(LIMITS, None, None, ConnectionLimits::from_config(&config))
                .expect("native backend must construct");
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
        let connection_key = ConnectionKey {
            scheme: "http".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: address.port(),
            dangerously_disable_tls_verification: false,
        };
        assert!(backend.reserve_connection(&connection_key));
        backend.transfers.insert(
            slot,
            HttpTransfer {
                request_id,
                response: TransferResponse::Buffered(ResponseDecoder::new(false, LIMITS)),
                body_bearing: true,
                response_started: false,
                connected: true,
                tls: None,
                connect_deadline: None,
                total_deadline: Some(deadline),
                inactivity_timeout: Some(Duration::from_secs(1)),
                inactivity_deadline: Some(deadline),
                inactivity_paused: false,
                key: connection_key,
                request_permits_reuse: true,
                request_write_drained: false,
                request: Request::get(format!("http://{address}/batch"))
                    .build()
                    .expect("batch request must build"),
                redirect_hops: 0,
                upload: None,
                upload_aborted: false,
            },
        );

        assert!(backend.reactor.cancel(slot));
        let completions = backend
            .process_events(vec![
                NativeEvent::WriteProgress(slot, 1),
                NativeEvent::Failed(
                    slot,
                    NativeFailure {
                        kind: NativeFailureKind::Read,
                        message: "simulated same-batch reset".to_owned(),
                        io_kind: None,
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

    #[test]
    fn standalone_terminal_socket_failure_dominates_same_batch_write_progress() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("TCP batch fixture must bind");
        let address = listener.local_addr().expect("TCP batch fixture address");
        let config = EngineConfig::manual();
        let mut backend =
            NativeHttpBackend::new(LIMITS, None, None, ConnectionLimits::from_config(&config))
                .expect("native backend must construct");
        let slot = backend
            .reactor
            .connect(address, None, 1024, 1024)
            .expect("TCP batch fixture must connect");
        let (_peer, _) = listener.accept().expect("TCP batch fixture must accept");
        let engine = crate::EngineBuilder::manual()
            .build()
            .expect("manual Engine must construct");
        let shared = engine.shared_for_testing();
        let request_id = RequestId {
            engine: shared.id,
            sequence: 1,
        };
        let (_io, owner) = crate::tcp::io::TcpIoShared::pair(crate::tcp::io::TcpIoConfig {
            engine_id: shared.id,
            request_id,
            shared,
            run_mode: crate::RunMode::Manual,
            send_window: 1024,
            receive_window: 1024,
            local: address,
            peer: address,
            engine_waker: Some(Arc::new(|| {})),
            on_release: Box::new(|| {}),
        });
        backend.standalone_request_to_slot.insert(request_id, slot);
        backend.standalone_live.insert(
            slot,
            StandaloneTcp::new(
                request_id,
                owner,
                Some(Duration::from_secs(1)),
                Some(Duration::from_secs(1)),
                Instant::now(),
            ),
        );

        assert!(backend.reactor.cancel(slot));
        backend
            .process_events(vec![
                NativeEvent::WriteProgress(slot, 1),
                NativeEvent::Failed(
                    slot,
                    NativeFailure {
                        kind: NativeFailureKind::Read,
                        message: "simulated standalone same-batch reset".to_owned(),
                        io_kind: Some(std::io::ErrorKind::ConnectionReset),
                    },
                ),
            ])
            .expect("same-batch standalone progress must not re-arm a removed socket");

        assert!(!backend.standalone_live.contains_key(&slot));
        assert!(!backend.standalone_request_to_slot.contains_key(&request_id));
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

    fn drain_until_socket_closed(stream: &mut std::net::TcpStream, context: &str) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("socket-close timeout must configure");
        let mut buffer = [0_u8; 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    return;
                }
                Err(error) => panic!("{context}: did not observe socket close: {error}"),
            }
        }
    }

    fn decode_fragmented(
        response_to_head: bool,
        bytes: &[u8],
        eof: bool,
    ) -> Result<Response, Error> {
        let mut decoder = ResponseDecoder::new(response_to_head, LIMITS);
        for byte in bytes {
            if let Some(response) = decoder.ingest(std::slice::from_ref(byte))?.response {
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
            if let Some(response) = decoder.ingest(part)?.response {
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
            b"POST /path?q=yes HTTP/1.1\r\nX-Binary: \x80x\r\nHost: example.test:8080\r\nContent-Length: 5\r\n\r\nhello"
        );
        assert!(!serialized.response_to_head);
        assert!(serialized.permits_reuse);

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
                header_count: 0,
                ..LIMITS
            },
        )
        .expect_err("generated Host must count");
        assert_eq!(error.limit_kind(), Some(LimitKind::RequestHeaderCount));
    }

    #[test]
    fn streamed_upload_serialization_generates_exact_framing_within_header_limits() {
        let request = Request::post("http://example.test/upload")
            .build()
            .expect("stream upload base request must build");
        let fixed =
            serialize_request_with_upload(&request, LIMITS, Some(UploadFraming::Fixed(123)))
                .expect("fixed stream head must serialize");
        assert_eq!(
            fixed.bytes,
            b"POST /upload HTTP/1.1\r\nHost: example.test\r\nContent-Length: 123\r\n\r\n"
        );
        let chunked = serialize_request_with_upload(&request, LIMITS, Some(UploadFraming::Chunked))
            .expect("chunked stream head must serialize");
        assert_eq!(
            chunked.bytes,
            b"POST /upload HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n"
        );
        let error = serialize_request_with_upload(
            &request,
            HttpLimits {
                header_count: 1,
                ..LIMITS
            },
            Some(UploadFraming::Fixed(1)),
        )
        .expect_err("generated Host plus framing must count as two fields");
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
    fn response_head_uses_stack_slots_then_honours_larger_configured_count() {
        let mut wire = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n".to_vec();
        for index in 1..40 {
            wire.extend_from_slice(format!("X-{index}: value\r\n").as_bytes());
        }
        wire.extend_from_slice(b"\r\n");
        let limits = HttpLimits {
            body_bytes: 1,
            header_bytes: 4096,
            header_count: 40,
        };
        let ParsedHead::Final {
            headers, framing, ..
        } = parse_response_head(&wire, false, limits)
            .expect("configured headers beyond the stack fast path must parse")
        else {
            panic!("response must have a final head")
        };
        assert_eq!(headers.len(), 40);
        assert!(matches!(framing, BodyFraming::Fixed(0)));

        let error = match parse_response_head(
            &wire,
            false,
            HttpLimits {
                header_count: 39,
                ..limits
            },
        ) {
            Err(error) => error,
            Ok(_) => panic!("the configured header-count limit must still fail closed"),
        };
        assert_eq!(error.limit_kind(), Some(LimitKind::ResponseHeaderCount));
    }

    #[test]
    fn decoder_reports_the_exact_boundary_before_trailing_bytes() {
        let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokjunk";
        let expected = wire.len() - b"junk".len();
        let mut decoder = ResponseDecoder::new(false, LIMITS);
        let progress = decoder.ingest(wire).expect("response must decode");
        let response = progress.response.expect("response must complete");
        assert_eq!(response.body(), b"ok");
        assert_eq!(progress.consumed, expected);
        assert_eq!(&wire[progress.consumed..], b"junk");
        assert!(progress.permits_reuse);
    }

    #[test]
    fn decoder_persistence_requires_unambiguous_http_connection_policy() {
        for (wire, expected) in [
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
                true,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
                false,
            ),
            (
                b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
                false,
            ),
            (
                b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n"
                    .as_slice(),
                true,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: upgrade\r\n\r\n".as_slice(),
                false,
            ),
        ] {
            let mut decoder = ResponseDecoder::new(false, LIMITS);
            let progress = decoder.ingest(wire).expect("policy response must decode");
            assert!(progress.response.is_some());
            assert_eq!(progress.permits_reuse, expected);
        }
    }

    #[test]
    fn native_http_reuses_one_clean_connection_for_sequential_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reuse fixture must bind");
        let address = listener.local_addr().expect("reuse fixture address");
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("reuse fixture must accept once");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("reuse fixture read timeout");
            for body in [b"one".as_slice(), b"two".as_slice()] {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 256];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).expect("reused request must read");
                    assert_ne!(read, 0, "client closed the reusable connection early");
                    request.extend_from_slice(&buffer[..read]);
                }
                stream
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                            .as_bytes(),
                    )
                    .expect("reused response head must write");
                stream
                    .write_all(body)
                    .expect("reused response body must write");
            }
            release_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("metrics observation must release the reusable peer");
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("reuse Engine must construct");
        for expected in [b"one".as_slice(), b"two".as_slice()] {
            let response = engine
                .client()
                .execute(
                    Request::get(format!("http://{address}/reuse"))
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("reuse request must build"),
                )
                .expect("reuse request must complete");
            assert_eq!(response.body(), expected);
        }
        let metrics = engine.metrics();
        assert_eq!(metrics.requests_accepted(), 2);
        assert_eq!(metrics.requests_completed(), 2);
        assert_eq!(metrics.connections_opened(), 1);
        assert_eq!(metrics.connections_reused(), 1);
        assert_eq!(metrics.connections_closed(), 0);
        assert_eq!(metrics.current().active_connections(), 1);
        assert_eq!(metrics.current().idle_connections(), 1);
        assert_eq!(metrics.high_water().active_connections(), 1);
        assert_eq!(metrics.high_water().idle_connections(), 1);
        release_tx.send(()).expect("reusable peer must release");
        engine.shutdown().expect("reuse Engine must stop");
        server.join().expect("reuse fixture must join");
    }

    #[test]
    fn configured_zero_idle_limits_or_timeout_disable_reuse() {
        for config in [
            EngineConfig::spawned().with_max_idle_connections(0),
            EngineConfig::spawned().with_max_idle_connections_per_origin(0),
            EngineConfig::spawned().with_idle_connection_timeout(Duration::ZERO),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("no-idle fixture must bind");
            let address = listener.local_addr().expect("no-idle fixture address");
            listener
                .set_nonblocking(true)
                .expect("no-idle listener must become nonblocking");
            let server = thread::spawn(move || {
                for body in [b"one".as_slice(), b"two".as_slice()] {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    let (mut stream, _) = loop {
                        match listener.accept() {
                            Ok(accepted) => break accepted,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    Instant::now() < deadline,
                                    "disabled idle retention did not open a replacement socket"
                                );
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(error) => panic!("no-idle accept failed: {error}"),
                        }
                    };
                    stream
                        .set_nonblocking(false)
                        .expect("no-idle accepted socket must become blocking");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .expect("no-idle socket timeout must configure");
                    read_request_head(&mut stream, "no-idle request");
                    stream
                        .write_all(
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                                .as_bytes(),
                        )
                        .expect("no-idle response head must write");
                    stream
                        .write_all(body)
                        .expect("no-idle response body must write");
                    assert_socket_closed(&mut stream, &mut [0_u8; 1], "no-idle retention");
                }
            });
            let engine = Engine::with_spawned_factory(
                config.clone(),
                Box::new(NativeHttpFactory::new(&config)),
            )
            .expect("no-idle Engine must construct");
            for expected in [b"one".as_slice(), b"two".as_slice()] {
                let response = engine
                    .client()
                    .execute(
                        Request::get(format!("http://{address}/no-idle"))
                            .total_timeout(Duration::from_secs(2))
                            .build()
                            .expect("no-idle request must build"),
                    )
                    .expect("no-idle request must complete");
                assert_eq!(response.body(), expected);
            }
            engine.shutdown().expect("no-idle Engine must stop");
            server.join().expect("no-idle fixture must join");
        }
    }

    #[test]
    fn manual_native_http_reuses_one_clean_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("manual reuse fixture must bind");
        let address = listener.local_addr().expect("manual reuse address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("manual reuse fixture must accept once");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("manual reuse read timeout");
            for body in [b"one".as_slice(), b"two".as_slice()] {
                read_request_head(&mut stream, "manual reused request");
                stream
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                            .as_bytes(),
                    )
                    .expect("manual reused response head must write");
                stream
                    .write_all(body)
                    .expect("manual reused response body must write");
                stream.flush().expect("manual reused response must flush");
            }
        });
        let config = EngineConfig::manual();
        let backend = NativeHttpBackend::new(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits::from_config(&config),
        )
        .expect("manual reuse backend must construct");
        let mut engine = Engine::with_backend(config, Box::new(backend))
            .expect("manual reuse Engine must construct");
        for expected in [b"one".as_slice(), b"two".as_slice()] {
            let pending = engine
                .client()
                .submit(
                    Request::get(format!("http://{address}/manual-reuse"))
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("manual reuse request must build"),
                )
                .expect("manual reuse request must submit");
            let Completion::Completed(response) = engine
                .drive_until(pending)
                .expect("manual reuse request must drive")
            else {
                panic!("manual reused request did not complete");
            };
            assert_eq!(response.body(), expected);
        }
        engine.shutdown().expect("manual reuse Engine must stop");
        server.join().expect("manual reuse fixture must join");
    }

    #[test]
    fn reused_protocol_or_limit_failure_closes_before_replacement() {
        #[derive(Clone, Copy)]
        enum Poison {
            Malformed,
            Oversize,
        }

        for poison in [Poison::Malformed, Poison::Oversize] {
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("reused contamination fixture must bind");
            let address = listener.local_addr().expect("reused contamination address");
            let server = thread::spawn(move || {
                let (mut first, _) = listener
                    .accept()
                    .expect("reused contamination first socket must accept");
                first
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("reused contamination first timeout");
                read_request_head(&mut first, "first clean request");
                first
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .expect("first clean response must write");
                first.flush().expect("first clean response must flush");
                read_request_head(&mut first, "reused poisoned request");
                let bytes = match poison {
                    Poison::Malformed => {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx"
                            .as_slice()
                    }
                    Poison::Oversize => {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nxxxx".as_slice()
                    }
                };
                first
                    .write_all(bytes)
                    .expect("poisoned response must write");
                first.flush().expect("poisoned response must flush");
                drop(first);

                let (mut replacement, _) = listener
                    .accept()
                    .expect("replacement socket must accept after poison");
                replacement
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("replacement timeout");
                read_request_head(&mut replacement, "replacement request");
                replacement
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew")
                    .expect("replacement response must write");
            });
            let config = EngineConfig::spawned().with_max_response_body_bytes(3);
            let engine = Engine::with_spawned_factory(
                config.clone(),
                Box::new(NativeHttpFactory::new(&config)),
            )
            .expect("reused contamination Engine must construct");
            let request = || {
                Request::get(format!("http://{address}/contamination"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("contamination request must build")
            };
            assert_eq!(
                engine
                    .client()
                    .execute(request())
                    .expect("first clean request must complete")
                    .body(),
                b"ok"
            );
            let Err(ExecuteError::Failed(error)) = engine.client().execute(request()) else {
                panic!("reused poisoned response did not fail");
            };
            match poison {
                Poison::Malformed => {
                    assert_eq!(error.kind(), ErrorKind::Transport);
                    assert_eq!(error.transport_stage(), Some(TransportStage::Http));
                }
                Poison::Oversize => {
                    assert_eq!(error.kind(), ErrorKind::Limit);
                    assert_eq!(error.limit_kind(), Some(LimitKind::ResponseBodyBytes));
                }
            }
            assert_eq!(
                engine
                    .client()
                    .execute(request())
                    .expect("replacement request must complete")
                    .body(),
                b"new"
            );
            engine
                .shutdown()
                .expect("reused contamination Engine must stop");
            server
                .join()
                .expect("reused contamination fixture must join");
        }
    }

    #[test]
    fn synthetic_idle_expiry_destroys_the_socket_and_releases_capacity() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("idle expiry fixture must bind");
        let address = listener.local_addr().expect("idle expiry address");
        let config = EngineConfig::manual().with_idle_connection_timeout(Duration::from_millis(17));
        let connection_limits = ConnectionLimits::from_config(&config);
        let mut backend = NativeHttpBackend::new(LIMITS, None, None, connection_limits)
            .expect("idle expiry backend must construct");
        let expires_at = Instant::now() + connection_limits.idle_timeout;
        let slot = backend
            .reactor
            .connect(address, Some(expires_at), 1, 1)
            .expect("idle expiry socket must connect");
        let (mut peer, _) = listener.accept().expect("idle expiry peer must accept");
        let key = ConnectionKey {
            scheme: "http".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: address.port(),
            dangerously_disable_tls_verification: false,
        };
        assert!(backend.reserve_connection(&key));
        backend.idle_slots.insert(slot, key.clone());
        backend
            .idle
            .entry(key)
            .or_default()
            .push_back(IdleConnection {
                slot,
                tls: None,
                expires_at,
            });
        backend.idle_count = 1;

        backend.expire_idle(expires_at);

        assert_eq!(backend.idle_count, 0);
        assert_eq!(backend.connection_count, 0);
        assert!(backend.idle.is_empty());
        assert!(backend.idle_slots.is_empty());
        assert!(peer_observed_close(&mut peer));
        backend.shutdown().expect("idle expiry backend must stop");
    }

    #[test]
    fn shutdown_closes_mixed_idle_and_leased_connections() {
        let idle_listener = TcpListener::bind("127.0.0.1:0").expect("mixed idle fixture must bind");
        let idle_address = idle_listener.local_addr().expect("mixed idle address");
        let idle_server = thread::spawn(move || {
            let (mut stream, _) = idle_listener
                .accept()
                .expect("mixed idle socket must accept");
            read_request_head(&mut stream, "mixed idle request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nidle")
                .expect("mixed idle response must write");
            stream.flush().expect("mixed idle response must flush");
            peer_observed_close(&mut stream)
        });

        let leased_listener =
            TcpListener::bind("127.0.0.1:0").expect("mixed leased fixture must bind");
        let leased_address = leased_listener.local_addr().expect("mixed leased address");
        let (leased_tx, leased_rx) = mpsc::channel();
        let leased_server = thread::spawn(move || {
            let (mut stream, _) = leased_listener
                .accept()
                .expect("mixed leased socket must accept");
            read_request_head(&mut stream, "mixed leased request");
            leased_tx
                .send(())
                .expect("mixed leased barrier must signal");
            peer_observed_close(&mut stream)
        });

        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("mixed shutdown Engine must construct");
        let idle = engine
            .client()
            .execute(
                Request::get(format!("http://{idle_address}/idle"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("mixed idle request must build"),
            )
            .expect("mixed idle request must complete");
        assert_eq!(idle.body(), b"idle");
        let leased = engine
            .client()
            .submit(
                Request::get(format!("http://{leased_address}/leased"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("mixed leased request must build"),
            )
            .expect("mixed leased request must submit");
        leased_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("mixed leased request must reach the peer");

        engine.shutdown().expect("mixed shutdown Engine must stop");

        assert!(matches!(leased.wait(), Completion::Cancelled));
        assert!(idle_server.join().expect("mixed idle fixture must join"));
        assert!(
            leased_server
                .join()
                .expect("mixed leased fixture must join")
        );
    }

    #[test]
    fn reused_peer_close_fails_without_transparent_replay() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("no-replay fixture must bind");
        let address = listener.local_addr().expect("no-replay fixture address");
        let (no_replay_tx, no_replay_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut first, _) = listener
                .accept()
                .expect("no-replay first socket must accept");
            first
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("no-replay first timeout");
            read_request_head(&mut first, "no-replay first request");
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("no-replay first response must write");
            first.flush().expect("no-replay first response must flush");
            read_request_head(&mut first, "no-replay reused request");
            drop(first);

            listener
                .set_nonblocking(true)
                .expect("no-replay listener must become nonblocking");
            let deadline = Instant::now() + Duration::from_millis(200);
            loop {
                match listener.accept() {
                    Ok(_) => panic!("failed reused request was transparently replayed"),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            break;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("no-replay accept probe failed: {error}"),
                }
            }
            listener
                .set_nonblocking(false)
                .expect("no-replay listener must return to blocking");
            no_replay_tx
                .send(())
                .expect("no-replay observation must signal");

            let (mut replacement, _) = listener
                .accept()
                .expect("explicit replacement request must open a socket");
            read_request_head(&mut replacement, "explicit replacement request");
            replacement
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew")
                .expect("explicit replacement response must write");
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("no-replay Engine must construct");
        let request = || {
            Request::get(format!("http://{address}/no-replay"))
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("no-replay request must build")
        };
        assert_eq!(
            engine
                .client()
                .execute(request())
                .expect("no-replay first request must complete")
                .body(),
            b"ok"
        );
        let Err(ExecuteError::Failed(error)) = engine.client().execute(request()) else {
            panic!("closed reused request did not fail");
        };
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert_eq!(error.transport_stage(), Some(TransportStage::Receive));
        no_replay_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server must observe no automatic replacement");
        assert_eq!(
            engine
                .client()
                .execute(request())
                .expect("explicit replacement request must complete")
                .body(),
            b"new"
        );
        engine.shutdown().expect("no-replay Engine must stop");
        server.join().expect("no-replay fixture must join");
    }

    #[test]
    fn native_redirects_apply_method_body_relative_and_limit_rules() {
        for status in [301_u16, 302] {
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("non-rewritten redirect fixture must bind");
            let address = listener
                .local_addr()
                .expect("non-rewritten redirect address");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener
                    .accept()
                    .expect("non-rewritten redirect must accept");
                let request = read_request_wire(&mut stream, "non-rewritten POST");
                assert!(request.starts_with(b"POST /start HTTP/1.1\r\n"));
                assert!(request.ends_with(b"payload"));
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} Redirect\r\nLocation: /final\r\nContent-Length: 0\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .expect("non-rewritten redirect must write");
            });
            let config = EngineConfig::spawned();
            let engine = Engine::with_spawned_factory(
                config.clone(),
                Box::new(NativeHttpFactory::new(&config)),
            )
            .expect("non-rewritten redirect Engine must construct");
            let response = engine
                .client()
                .execute(
                    Request::post(format!("http://{address}/start"))
                        .body(b"payload".to_vec())
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("non-rewritten redirect request must build"),
                )
                .expect("301/302 POST must remain a completed response");
            assert_eq!(response.status(), status);
            engine
                .shutdown()
                .expect("non-rewritten redirect Engine must stop");
            server
                .join()
                .expect("non-rewritten redirect fixture must join");
        }

        for (status, expected_method, expected_body) in [
            (303_u16, "GET", b"".as_slice()),
            (307_u16, "POST", b"payload".as_slice()),
            (308_u16, "POST", b"payload".as_slice()),
        ] {
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("followed redirect fixture must bind");
            let address = listener.local_addr().expect("followed redirect address");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener
                    .accept()
                    .expect("followed redirect must accept once");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("followed redirect read timeout");
                let first = read_request_wire(&mut stream, "redirect source request");
                assert!(first.starts_with(b"POST /base/start HTTP/1.1\r\n"));
                assert!(first.ends_with(b"payload"));
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} Redirect\r\nLocation: ../final?x=1\r\nContent-Length: 0\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .expect("followed redirect must write");
                stream.flush().expect("followed redirect must flush");
                let second = read_request_wire(&mut stream, "redirect target request");
                assert!(
                    second.starts_with(
                        format!("{expected_method} /final?x=1 HTTP/1.1\r\n").as_bytes()
                    )
                );
                assert_eq!(
                    &second[second
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .expect("redirect target head boundary")
                        + 4..],
                    expected_body
                );
                let lower = String::from_utf8_lossy(&second).to_ascii_lowercase();
                assert!(lower.contains("authorization: same-origin"));
                if status == 303 {
                    assert!(!lower.contains("content-length:"));
                }
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nfinal")
                    .expect("redirect final response must write");
            });
            let config = EngineConfig::spawned();
            let engine = Engine::with_spawned_factory(
                config.clone(),
                Box::new(NativeHttpFactory::new(&config)),
            )
            .expect("followed redirect Engine must construct");
            let response = engine
                .client()
                .execute(
                    Request::post(format!("http://{address}/base/start"))
                        .header("Authorization", "same-origin")
                        .body(b"payload".to_vec())
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("followed redirect request must build"),
                )
                .expect("redirect must complete");
            assert_eq!(response.body(), b"final");
            engine
                .shutdown()
                .expect("followed redirect Engine must stop");
            server.join().expect("followed redirect fixture must join");
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("redirect loop must bind");
        let address = listener.local_addr().expect("redirect loop address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("redirect loop must accept once");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("redirect loop read timeout");
            for _ in 0..3 {
                let request = read_request_wire(&mut stream, "redirect loop request");
                assert!(request.starts_with(b"GET /loop HTTP/1.1\r\n"));
                stream
                    .write_all(
                        b"HTTP/1.1 302 Redirect\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n",
                    )
                    .expect("redirect loop response must write");
                stream.flush().expect("redirect loop response must flush");
            }
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("redirect loop Engine must construct");
        let result = engine.client().execute(
            Request::get(format!("http://{address}/loop"))
                .redirect_limit(2)
                .total_timeout(Duration::from_secs(2))
                .build()
                .expect("redirect loop request must build"),
        );
        let Err(ExecuteError::Failed(error)) = result else {
            panic!("redirect loop did not fail at its hop limit");
        };
        assert_eq!(error.kind(), ErrorKind::Redirect);
        engine.shutdown().expect("redirect loop Engine must stop");
        server.join().expect("redirect loop fixture must join");
    }

    #[test]
    fn native_cross_origin_redirect_strips_origin_bound_credentials() {
        let target_listener = TcpListener::bind("127.0.0.1:0").expect("redirect target must bind");
        let target_address = target_listener
            .local_addr()
            .expect("redirect target address");
        let target_server = thread::spawn(move || {
            let (mut stream, _) = target_listener
                .accept()
                .expect("redirect target must accept");
            let request = read_request_wire(&mut stream, "cross-origin redirect target");
            let lower = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(request.starts_with(b"GET /target HTTP/1.1\r\n"));
            assert!(!lower.contains("authorization:"));
            assert!(!lower.contains("proxy-authorization:"));
            assert!(!lower.contains("cookie:"));
            assert!(!lower.contains("host: caller.invalid"));
            assert!(lower.contains("x-keep: yes"));
            assert!(lower.contains(&format!("host: {target_address}")));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\ntarget")
                .expect("redirect target response must write");
        });

        let source_listener = TcpListener::bind("127.0.0.1:0").expect("redirect source must bind");
        let source_address = source_listener
            .local_addr()
            .expect("redirect source address");
        let source_server = thread::spawn(move || {
            let (mut stream, _) = source_listener
                .accept()
                .expect("redirect source must accept");
            let request = read_request_wire(&mut stream, "cross-origin redirect source");
            let lower = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(lower.contains("authorization: secret"));
            assert!(lower.contains("proxy-authorization: proxy-secret"));
            assert!(lower.contains("cookie: session=secret"));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 302 Redirect\r\nLocation: http://{target_address}/target\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .expect("cross-origin redirect must write");
        });

        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("cross-origin redirect Engine must construct");
        let response = engine
            .client()
            .execute(
                Request::get(format!("http://{source_address}/source"))
                    .header("Authorization", "secret")
                    .header("Proxy-Authorization", "proxy-secret")
                    .header("Cookie", "session=secret")
                    .header("Host", "caller.invalid")
                    .header("X-Keep", "yes")
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("cross-origin redirect request must build"),
            )
            .expect("cross-origin redirect must complete");
        assert_eq!(response.body(), b"target");
        engine
            .shutdown()
            .expect("cross-origin redirect Engine must stop");
        source_server
            .join()
            .expect("redirect source fixture must join");
        target_server
            .join()
            .expect("redirect target fixture must join");
    }

    #[test]
    fn native_redirect_location_rejects_ambiguity_and_invalid_values() {
        let request = Request::get("http://example.test/base/start")
            .build()
            .expect("redirect source must build");
        let missing = Response::new(302, Vec::new(), Vec::new());
        assert_eq!(
            resolved_redirect_target(&request, &missing)
                .expect("missing Location must be a completed redirect response"),
            None
        );

        let duplicate = Response::new(
            302,
            vec![
                crate::Header::new("Location", "/one"),
                crate::Header::new("location", "/two"),
            ],
            Vec::new(),
        );
        let error = resolved_redirect_target(&request, &duplicate)
            .expect_err("duplicate Location must fail closed");
        assert_eq!(error.kind(), ErrorKind::Redirect);

        let invalid_utf8 = Response::new(
            302,
            vec![crate::Header::new("Location", vec![0xff])],
            Vec::new(),
        );
        let error = resolved_redirect_target(&request, &invalid_utf8)
            .expect_err("non-UTF-8 Location must fail closed");
        assert_eq!(error.kind(), ErrorKind::Redirect);

        let unsupported = Response::new(
            302,
            vec![crate::Header::new("Location", "ftp://example.test/file")],
            Vec::new(),
        );
        let target = resolved_redirect_target(&request, &unsupported)
            .expect("absolute Location must resolve")
            .expect("Location must be present");
        let error = redirected_request(&request, 302, 0, || Ok(Some(target)))
            .expect_err("unsupported redirect scheme must fail closed");
        assert_eq!(error.kind(), ErrorKind::Redirect);
    }

    #[test]
    fn cancel_during_redirected_request_closes_the_active_hop() {
        let target_listener =
            TcpListener::bind("127.0.0.1:0").expect("redirect cancel target must bind");
        let target_address = target_listener
            .local_addr()
            .expect("redirect cancel target address");
        let (active_tx, active_rx) = mpsc::channel();
        let target_server = thread::spawn(move || {
            let (mut stream, _) = target_listener
                .accept()
                .expect("redirect cancel target must accept");
            read_request_head(&mut stream, "redirect cancel target request");
            active_tx
                .send(())
                .expect("redirect cancel barrier must signal");
            assert!(
                peer_observed_close(&mut stream),
                "redirect cancellation must close the active target socket"
            );
        });

        let source_listener =
            TcpListener::bind("127.0.0.1:0").expect("redirect cancel source must bind");
        let source_address = source_listener
            .local_addr()
            .expect("redirect cancel source address");
        let source_server = thread::spawn(move || {
            let (mut stream, _) = source_listener
                .accept()
                .expect("redirect cancel source must accept");
            read_request_head(&mut stream, "redirect cancel source request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 302 Redirect\r\nLocation: http://{target_address}/stall\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .expect("redirect cancel source must write");
        });

        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("redirect cancel Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::get(format!("http://{source_address}/start"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("redirect cancel request must build"),
            )
            .expect("redirect cancel request must submit");
        active_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("redirect target must become active");
        pending
            .handle()
            .cancel()
            .expect("redirected request must cancel");
        assert!(matches!(pending.wait(), Completion::Cancelled));
        engine.shutdown().expect("redirect cancel Engine must stop");
        source_server
            .join()
            .expect("redirect cancel source must join");
        target_server
            .join()
            .expect("redirect cancel target must join");
    }

    #[test]
    fn total_timeout_is_not_reset_between_redirect_hops() {
        let target_listener =
            TcpListener::bind("127.0.0.1:0").expect("redirect timeout target must bind");
        let target_address = target_listener
            .local_addr()
            .expect("redirect timeout target address");
        let target_server = thread::spawn(move || {
            let (mut stream, _) = target_listener
                .accept()
                .expect("redirect timeout target must accept");
            read_request_head(&mut stream, "redirect timeout target request");
            assert!(
                peer_observed_close(&mut stream),
                "total timeout must close the redirected target socket"
            );
        });

        let source_listener =
            TcpListener::bind("127.0.0.1:0").expect("redirect timeout source must bind");
        let source_address = source_listener
            .local_addr()
            .expect("redirect timeout source address");
        let source_server = thread::spawn(move || {
            let (mut stream, _) = source_listener
                .accept()
                .expect("redirect timeout source must accept");
            read_request_head(&mut stream, "redirect timeout source request");
            thread::sleep(Duration::from_millis(140));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 302 Redirect\r\nLocation: http://{target_address}/stall\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .expect("redirect timeout source must write");
        });

        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("redirect timeout Engine must construct");
        let started = Instant::now();
        let result = engine.client().execute(
            Request::get(format!("http://{source_address}/start"))
                .total_timeout(Duration::from_millis(220))
                .build()
                .expect("redirect timeout request must build"),
        );
        let elapsed = started.elapsed();
        let Err(ExecuteError::Failed(error)) = result else {
            panic!("redirected request did not retain its original total deadline");
        };
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
        assert!(
            elapsed < Duration::from_millis(320),
            "redirect reset the total timeout: {elapsed:?}"
        );
        engine
            .shutdown()
            .expect("redirect timeout Engine must stop");
        source_server
            .join()
            .expect("redirect timeout source must join");
        target_server
            .join()
            .expect("redirect timeout target must join");
    }

    #[test]
    fn response_connection_close_forces_the_next_request_onto_a_new_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("close fixture must bind");
        listener
            .set_nonblocking(true)
            .expect("close fixture must become nonblocking");
        let address = listener.local_addr().expect("close fixture address");
        let server = thread::spawn(move || {
            for index in 0..2 {
                let deadline = Instant::now() + Duration::from_secs(2);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(accepted) => break accepted,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "next socket was not opened");
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("close fixture accept failed: {error}"),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .expect("close fixture stream must be blocking");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 256];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).expect("close request must read");
                    assert_ne!(read, 0, "client closed before the close-policy request");
                    request.extend_from_slice(&buffer[..read]);
                }
                let connection = if index == 0 {
                    "Connection: close\r\n"
                } else {
                    ""
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: 1\r\n{connection}\r\n{}",
                            index + 1
                        )
                        .as_bytes(),
                    )
                    .expect("close-policy response must write");
            }
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("close-policy Engine must construct");
        for expected in [b"1".as_slice(), b"2".as_slice()] {
            let response = engine
                .client()
                .execute(
                    Request::get(format!("http://{address}/close"))
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("close-policy request must build"),
                )
                .expect("close-policy request must complete");
            assert_eq!(response.body(), expected);
        }
        engine.shutdown().expect("close-policy Engine must stop");
        server.join().expect("close fixture must join");
    }

    #[test]
    fn idle_peer_close_is_evicted_before_the_next_request() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("idle-close fixture must bind");
        listener
            .set_nonblocking(true)
            .expect("idle-close fixture must become nonblocking");
        let address = listener.local_addr().expect("idle-close fixture address");
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            for index in 0..2 {
                let deadline = Instant::now() + Duration::from_secs(2);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(accepted) => break accepted,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "replacement socket was not opened"
                            );
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("idle-close fixture accept failed: {error}"),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .expect("idle-close fixture stream must be blocking");
                stream
                    .set_nonblocking(false)
                    .expect("idle-close peer must become blocking");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 256];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream
                        .read(&mut buffer)
                        .expect("idle-close request must read");
                    assert_ne!(read, 0, "client closed before idle-close request");
                    request.extend_from_slice(&buffer[..read]);
                }
                stream
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n{}", index + 1)
                            .as_bytes(),
                    )
                    .expect("idle-close response must write");
                stream.flush().expect("idle-close response must flush");
                if index == 0 {
                    drop(stream);
                    closed_tx.send(()).expect("peer close barrier must signal");
                }
            }
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("idle-close Engine must construct");
        let first = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/idle-close"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("first idle-close request must build"),
            )
            .expect("first idle-close request must complete");
        assert_eq!(first.body(), b"1");
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server must close the idle peer");
        thread::sleep(NATIVE_SAFETY_POLL * 3);
        let second = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/idle-close"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("second idle-close request must build"),
            )
            .expect("second idle-close request must use a replacement socket");
        assert_eq!(second.body(), b"2");
        let metrics = engine.metrics();
        assert_eq!(metrics.connections_opened(), 2);
        assert_eq!(metrics.connections_reused(), 0);
        assert!(metrics.connections_closed() >= 1);
        assert!(metrics.idle_connections_evicted() >= 1);
        engine.shutdown().expect("idle-close Engine must stop");
        server.join().expect("idle-close fixture must join");
    }

    #[test]
    fn cancelling_a_reused_request_closes_it_before_the_next_lease() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reuse-cancel fixture must bind");
        let address = listener.local_addr().expect("reuse-cancel fixture address");
        let (second_tx, second_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let read_head = |stream: &mut std::net::TcpStream| {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 256];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream
                        .read(&mut buffer)
                        .expect("reuse-cancel request must read");
                    assert_ne!(read, 0, "client closed before the complete request head");
                    request.extend_from_slice(&buffer[..read]);
                }
            };

            let (mut first, _) = listener
                .accept()
                .expect("first reusable socket must accept");
            first
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("first reusable socket timeout");
            read_head(&mut first);
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\none")
                .expect("first reusable response must write");
            first.flush().expect("first reusable response must flush");

            read_head(&mut first);
            second_tx
                .send(())
                .expect("second reused request barrier must signal");
            let mut byte = [0_u8; 1];
            match first.read(&mut byte) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) => {}
                other => panic!("cancelled reused socket was not closed: {other:?}"),
            }

            let (mut replacement, _) = listener
                .accept()
                .expect("replacement socket after cancellation must accept");
            replacement
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("replacement socket timeout");
            read_head(&mut replacement);
            replacement
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nthree")
                .expect("replacement response must write");
        });
        let config = EngineConfig::spawned();
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("reuse-cancel Engine must construct");
        let first = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/one"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("first reusable request must build"),
            )
            .expect("first reusable request must complete");
        assert_eq!(first.body(), b"one");
        let second = engine
            .client()
            .submit(
                Request::get(format!("http://{address}/two"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("second reusable request must build"),
            )
            .expect("second reusable request must submit");
        second_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server must observe the reused request");
        second
            .handle()
            .cancel()
            .expect("reused request must cancel");
        assert!(matches!(second.wait(), Completion::Cancelled));
        let third = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/three"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("replacement request must build"),
            )
            .expect("replacement request must complete");
        assert_eq!(third.body(), b"three");
        engine.shutdown().expect("reuse-cancel Engine must stop");
        server.join().expect("reuse-cancel fixture must join");
    }

    #[test]
    fn manual_lease_probe_rejects_unobserved_bytes_before_reuse() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("lease-probe fixture must bind");
        let address = listener.local_addr().expect("lease-probe fixture address");
        let (inject_tx, inject_rx) = std::sync::mpsc::channel();
        let (injected_tx, injected_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let read_head = |stream: &mut std::net::TcpStream| {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 256];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream
                        .read(&mut buffer)
                        .expect("lease-probe request must read");
                    assert_ne!(read, 0, "client closed before lease-probe request head");
                    request.extend_from_slice(&buffer[..read]);
                }
            };

            let (mut first, _) = listener
                .accept()
                .expect("first lease-probe socket must accept");
            first
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("first lease-probe timeout");
            read_head(&mut first);
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\none")
                .expect("first lease-probe response must write");
            first
                .flush()
                .expect("first lease-probe response must flush");
            inject_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("test must release the injected bytes");
            first
                .write_all(b"HTTP/1.1 299 Poison\r\nContent-Length: 4\r\n\r\nfake")
                .expect("unobserved bytes must write");
            first.flush().expect("unobserved bytes must flush");
            injected_tx
                .send(())
                .expect("unobserved-byte barrier must signal");

            let mut byte = [0_u8; 1];
            match first.read(&mut byte) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) => {}
                Ok(_) => return false,
                Err(error) => panic!("lease probe did not close poisoned socket: {error}"),
            }

            let (mut replacement, _) = listener
                .accept()
                .expect("clean replacement socket must accept");
            replacement
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("replacement lease-probe timeout");
            read_head(&mut replacement);
            replacement
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ngood")
                .expect("replacement lease-probe response must write");
            true
        });

        let config = EngineConfig::manual();
        let backend = NativeHttpBackend::new(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits::from_config(&config),
        )
        .expect("manual lease-probe backend must construct");
        let mut engine = Engine::with_backend(config, Box::new(backend))
            .expect("manual lease-probe Engine must construct");
        let first = engine
            .client()
            .submit(
                Request::get(format!("http://{address}/one"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("first lease-probe request must build"),
            )
            .expect("first lease-probe request must submit");
        let Completion::Completed(first) = engine
            .drive_until(first)
            .expect("first lease-probe request must drive")
        else {
            panic!("first lease-probe request did not complete");
        };
        assert_eq!(first.body(), b"one");
        inject_tx.send(()).expect("injected bytes must release");
        injected_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server must inject bytes before the next drive");

        let second = engine
            .client()
            .submit(
                Request::get(format!("http://{address}/two"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("second lease-probe request must build"),
            )
            .expect("second lease-probe request must submit");
        let Completion::Completed(second) = engine
            .drive_until(second)
            .expect("second lease-probe request must drive")
        else {
            panic!("second lease-probe request did not complete");
        };
        assert_eq!(second.status(), 200);
        assert_eq!(second.body(), b"good");
        engine.shutdown().expect("lease-probe Engine must stop");
        assert!(server.join().expect("lease-probe fixture must join"));
    }

    #[test]
    fn connection_queue_starts_the_oldest_eligible_origin_without_starving_its_head() {
        let listener_a = TcpListener::bind("127.0.0.1:0").expect("origin A must bind");
        let address_a = listener_a.local_addr().expect("origin A address");
        let listener_b = TcpListener::bind("127.0.0.1:0").expect("origin B must bind");
        let address_b = listener_b.local_addr().expect("origin B address");
        let (a_ready_tx, a_ready_rx) = std::sync::mpsc::channel();
        let (release_a_tx, release_a_rx) = std::sync::mpsc::channel();
        let server_a = thread::spawn(move || {
            let read_head = |stream: &mut std::net::TcpStream| {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 256];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream
                        .read(&mut buffer)
                        .expect("origin A request must read");
                    assert_ne!(read, 0, "origin A closed before request head");
                    request.extend_from_slice(&buffer[..read]);
                }
            };
            let (mut stream, _) = listener_a.accept().expect("origin A must accept once");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("origin A timeout");
            read_head(&mut stream);
            a_ready_tx.send(()).expect("origin A barrier must signal");
            release_a_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("origin A must be released");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na1")
                .expect("origin A first response must write");
            stream.flush().expect("origin A first response must flush");
            read_head(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na2")
                .expect("origin A second response must write");
        });
        let server_b = thread::spawn(move || {
            let (mut stream, _) = listener_b.accept().expect("origin B must accept");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .expect("origin B request must read");
                assert_ne!(read, 0, "origin B closed before request head");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nb1")
                .expect("origin B response must write");
        });

        let config = EngineConfig::manual()
            .with_max_connections(std::num::NonZeroUsize::new(2).expect("two is non-zero"))
            .with_max_connections_per_origin(
                std::num::NonZeroUsize::new(1).expect("one is non-zero"),
            );
        let backend = NativeHttpBackend::new_with_connection_limits(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits::from_config(&config),
        )
        .expect("bounded native backend must construct");
        let mut engine = Engine::with_backend(config, Box::new(backend))
            .expect("bounded manual Engine must construct");
        let request = |url: String| {
            Request::get(url)
                .total_timeout(Duration::from_secs(3))
                .build()
                .expect("bounded request must build")
        };
        let a1 = engine
            .client()
            .submit(request(format!("http://{address_a}/a1")))
            .expect("A1 must submit");
        let a2 = engine
            .client()
            .submit(request(format!("http://{address_a}/a2")))
            .expect("A2 must submit");
        let b1 = engine
            .client()
            .submit(request(format!("http://{address_b}/b1")))
            .expect("B1 must submit");
        let Completion::Completed(b1) = engine.drive_until(b1).expect("B1 must drive") else {
            panic!("oldest eligible request on origin B did not complete");
        };
        assert_eq!(b1.body(), b"b1");
        a_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("A1 must already own its connection");
        release_a_tx.send(()).expect("A1 must release");
        let Completion::Completed(a1) = engine.drive_until(a1).expect("A1 must drive") else {
            panic!("A1 did not complete");
        };
        assert_eq!(a1.body(), b"a1");
        let Completion::Completed(a2) = engine.drive_until(a2).expect("A2 must drive") else {
            panic!("queued A2 did not complete");
        };
        assert_eq!(a2.body(), b"a2");
        engine.shutdown().expect("bounded Engine must stop");
        server_a.join().expect("origin A fixture must join");
        server_b.join().expect("origin B fixture must join");
    }

    #[test]
    fn connection_queue_preserves_acceptance_timeouts_without_opening_past_the_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("queue-timeout fixture must bind");
        let address = listener
            .local_addr()
            .expect("queue-timeout fixture address");
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("queue-timeout fixture must accept once");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("queue-timeout socket timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 256];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .expect("first capped request must read");
                assert_ne!(read, 0, "first capped request closed too soon");
                request.extend_from_slice(&buffer[..read]);
            }
            first_tx.send(()).expect("first capped barrier must signal");
            match stream.read(&mut buffer) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) => {}
                other => panic!("first capped socket was not closed: {other:?}"),
            }
        });
        let config = EngineConfig::manual()
            .with_max_connections(std::num::NonZeroUsize::new(1).expect("one is non-zero"))
            .with_max_connections_per_origin(
                std::num::NonZeroUsize::new(1).expect("one is non-zero"),
            );
        let backend = NativeHttpBackend::new_with_connection_limits(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits::from_config(&config),
        )
        .expect("queue-timeout backend must construct");
        let mut engine = Engine::with_backend(config, Box::new(backend))
            .expect("queue-timeout Engine must construct");
        let first = engine
            .client()
            .submit(
                Request::get(format!("http://{address}/first"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("first capped request must build"),
            )
            .expect("first capped request must submit");
        let second = engine
            .client()
            .submit(
                Request::get(format!("http://{address}/second"))
                    .total_timeout(Duration::from_millis(100))
                    .build()
                    .expect("queued timeout request must build"),
            )
            .expect("queued timeout request must submit");
        let Completion::Failed(error) = engine
            .drive_until(second)
            .expect("queued timeout request must drive")
        else {
            panic!("queued request did not time out");
        };
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
        let metrics = engine.metrics();
        assert_eq!(metrics.requests_accepted(), 2);
        assert_eq!(metrics.requests_failed(), 1);
        assert_eq!(metrics.current().active_connections(), 1);
        assert_eq!(metrics.current().connection_waiters(), 0);
        assert_eq!(metrics.high_water().connection_waiters(), 1);
        first_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first capped request must own the only connection");
        first
            .handle()
            .cancel()
            .expect("first capped request must cancel");
        assert!(matches!(first.wait(), Completion::Cancelled));
        engine.shutdown().expect("queue-timeout Engine must stop");
        server.join().expect("queue-timeout fixture must join");
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
    fn public_stream_reader_drains_a_backpressured_native_cleartext_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stream fixture must bind");
        let address = listener.local_addr().expect("stream fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("stream fixture must accept");
            read_request_head(&mut stream, "stream request head");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabcdefghij")
                .expect("stream response must write");
        });

        let config = EngineConfig::spawned()
            .with_max_stream_queue_bytes_per_request(3)
            .with_max_stream_queued_bytes(3);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("native streaming Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("http://{address}/stream"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("stream request must build"),
            )
            .expect("native stream request must submit");
        assert_eq!(
            reader
                .wait_head()
                .expect("stream head must arrive")
                .status(),
            200
        );
        let mut received = Vec::new();
        let mut hole = [0_u8; 2];
        while let Some(read) = reader.read(&mut hole).expect("stream body must read") {
            received.extend_from_slice(&hole[..read]);
        }
        assert_eq!(received, b"abcdefghij");
        engine
            .shutdown()
            .expect("native streaming Engine must stop");
        server.join().expect("stream fixture must join");
    }

    #[test]
    fn fixed_streamed_upload_pumps_incrementally_over_cleartext() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixed upload fixture must bind");
        let address = listener.local_addr().expect("fixed upload address");
        let (respond_tx, respond_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fixed upload must accept");
            let request = read_request_wire(&mut stream, "fixed streamed upload");
            let head_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("fixed upload head delimiter")
                + 4;
            let head = std::str::from_utf8(&request[..head_end]).expect("fixed head is UTF-8");
            assert!(head.contains("Content-Length: 6\r\n"));
            assert!(!head.to_ascii_lowercase().contains("transfer-encoding"));
            assert_eq!(&request[head_end..], b"abcdef");
            respond_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("finished producer must release the fixed response");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("fixed upload response must write");
        });

        let (body, mut sender) = UploadBody::fixed(6, 3).expect("fixed pair must construct");
        sender
            .try_push(b"abc".to_vec())
            .expect("first fixed chunk fits");
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(3);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("fixed upload Engine must construct");
        let reader = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("http://{address}/fixed"))
                    .body_stream(body)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("fixed streamed request must build"),
            )
            .expect("fixed streamed request must submit");
        push_eventually(&mut sender, b"def".to_vec(), "second fixed chunk");
        sender.finish().expect("fixed sender must finish exactly");
        respond_tx
            .send(())
            .expect("fixed response gate must remain available");
        let response = reader.collect().expect("fixed response must collect");
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"ok");
        engine.shutdown().expect("fixed upload Engine must stop");
        server.join().expect("fixed upload fixture must join");
    }

    #[test]
    fn chunked_streamed_upload_generates_framing_and_terminal_chunk() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("chunked upload fixture must bind");
        let address = listener.local_addr().expect("chunked upload address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("chunked upload must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("chunked fixture timeout must configure");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 128];
            while !request.windows(5).any(|window| window == b"0\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("chunked upload must read");
                assert_ne!(read, 0, "chunked upload closed before terminal chunk");
                request.extend_from_slice(&buffer[..read]);
            }
            let head_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("chunked upload head delimiter")
                + 4;
            let head = std::str::from_utf8(&request[..head_end]).expect("chunked head is UTF-8");
            assert!(head.contains("Transfer-Encoding: chunked\r\n"));
            assert!(!head.to_ascii_lowercase().contains("content-length"));
            assert_eq!(&request[head_end..], b"2\r\nab\r\n3\r\ncde\r\n0\r\n\r\n");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                .expect("chunked upload response must write");
        });

        let (body, mut sender) = UploadBody::chunked(4).expect("chunked pair must construct");
        sender
            .try_push(b"ab".to_vec())
            .expect("first chunked chunk fits");
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("chunked upload Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("http://{address}/chunked"))
                    .body_stream(body)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("chunked streamed request must build"),
            )
            .expect("chunked streamed request must submit");
        push_eventually(&mut sender, b"cde".to_vec(), "second chunked chunk");
        sender.finish().expect("chunked sender must finish");
        assert_eq!(
            reader.wait_head().expect("chunked response head").status(),
            204
        );
        assert!(reader.is_eof());
        engine.shutdown().expect("chunked upload Engine must stop");
        server.join().expect("chunked upload fixture must join");
    }

    #[test]
    fn streamed_upload_redirect_is_returned_unfollowed() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("upload redirect must bind");
        let address = listener.local_addr().expect("upload redirect address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("upload redirect must accept once");
            let request = read_request_wire(&mut stream, "upload redirect request");
            assert!(request.ends_with(b"data"));
            stream
                .write_all(
                    b"HTTP/1.1 303 See Other\r\nContent-Length: 0\r\nLocation: /must-not-follow\r\n\r\n",
                )
                .expect("upload redirect response must write");
            listener
                .set_nonblocking(true)
                .expect("upload redirect listener must become nonblocking");
            thread::sleep(Duration::from_millis(100));
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                "a live upload redirect must not open another request"
            );
        });

        let (body, mut sender) = UploadBody::fixed(4, 4).expect("upload redirect pair");
        sender
            .try_push(b"data".to_vec())
            .expect("upload redirect data fits");
        sender.finish().expect("upload redirect producer finishes");
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("upload redirect Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("http://{address}/upload"))
                    .body_stream(body)
                    .redirect_limit(5)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("upload redirect request must build"),
            )
            .expect("upload redirect request must submit");
        assert_eq!(
            reader
                .wait_head()
                .expect("redirect head must arrive")
                .status(),
            303
        );
        assert!(reader.is_eof());
        engine.shutdown().expect("upload redirect Engine must stop");
        server.join().expect("upload redirect fixture must join");
    }

    #[test]
    fn completed_streamed_upload_can_return_a_clean_connection_to_the_pool() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("upload reuse fixture must bind");
        let address = listener.local_addr().expect("upload reuse address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("upload reuse must accept once");
            let first = read_request_wire(&mut stream, "streamed upload before reuse");
            assert!(first.ends_with(b"data"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("upload reuse first response must write");
            let second = read_request_wire(&mut stream, "request after streamed upload");
            assert!(second.starts_with(b"GET /after HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nafter")
                .expect("upload reuse second response must write");
        });

        let (body, mut sender) = UploadBody::fixed(4, 4).expect("upload reuse pair");
        sender
            .try_push(b"data".to_vec())
            .expect("upload reuse data fits");
        sender.finish().expect("upload reuse sender finishes");
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("upload reuse Engine must construct");
        let first = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("http://{address}/upload"))
                    .body_stream(body)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("upload reuse stream request must build"),
            )
            .expect("upload reuse stream request must submit")
            .collect()
            .expect("upload reuse stream response must collect");
        assert_eq!(first.body(), b"ok");
        let second = engine
            .client()
            .execute(
                Request::get(format!("http://{address}/after"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("upload reuse second request must build"),
            )
            .expect("upload reuse second request must complete");
        assert_eq!(second.body(), b"after");
        engine.shutdown().expect("upload reuse Engine must stop");
        server.join().expect("upload reuse fixture must join");
    }

    #[test]
    fn early_final_response_closes_streamed_upload_and_keeps_http_result() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("early response fixture must bind");
        let address = listener.local_addr().expect("early response address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("early response must accept");
            read_request_head(&mut stream, "early response request head");
            stream
                .write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 6\r\n\r\nreject")
                .expect("early response must write");
            drain_until_socket_closed(&mut stream, "early final response");
        });

        const EARLY_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
        let (body, mut sender) =
            UploadBody::fixed(EARLY_UPLOAD_BYTES as u64, 3).expect("early upload pair");
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(3);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("early response Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("http://{address}/reject"))
                    .body_stream(body)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("early response request must build"),
            )
            .expect("early response request must submit");
        let producer = thread::spawn(move || sender.push(vec![b'x'; EARLY_UPLOAD_BYTES]));
        assert_eq!(reader.wait_head().expect("413 head must win").status(), 413);
        let error = producer
            .join()
            .expect("early producer thread must join")
            .expect_err("early response must wake and close blocking producer");
        assert_eq!(error.kind(), TryPushErrorKind::Closed);
        assert!(!error.into_chunk().is_empty());
        let response = reader
            .collect()
            .expect("early response body must remain readable under backpressure");
        assert_eq!(response.body(), b"reject");
        engine.shutdown().expect("early response Engine must stop");
        server.join().expect("early response fixture must join");
    }

    #[test]
    fn streaming_consumer_backpressure_pauses_inactivity_but_not_delivery() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("pressure fixture must bind");
        let address = listener.local_addr().expect("pressure fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("pressure fixture must accept");
            read_request_head(&mut stream, "pressure request head");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabcdef")
                .expect("pressure response must write");
        });

        let config = EngineConfig::spawned()
            .with_max_stream_queue_bytes_per_request(3)
            .with_max_stream_queued_bytes(3);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("pressure Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("http://{address}/pressure"))
                    .inactivity_timeout(Duration::from_millis(75))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("pressure request must build"),
            )
            .expect("pressure request must submit");
        reader.wait_head().expect("pressure head must arrive");
        let fill_deadline = Instant::now() + Duration::from_secs(1);
        while reader.queued_bytes_for_test() != 3 {
            assert!(
                Instant::now() < fill_deadline,
                "pressure response queue did not reach its exact full state"
            );
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(225));
        let response = reader
            .collect()
            .expect("consumer backpressure must suppress inactivity timeout");
        assert_eq!(response.body(), b"abcdef");
        engine.shutdown().expect("pressure Engine must stop");
        server.join().expect("pressure fixture must join");
    }

    #[test]
    fn streaming_consumer_backpressure_does_not_pause_total_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("total fixture must bind");
        let address = listener.local_addr().expect("total fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("total fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("total fixture timeout must configure");
            read_request_head(&mut stream, "total request head");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc")
                .expect("total partial response must write");
            assert_socket_closed(&mut stream, &mut [0_u8; 1], "stream total timeout");
        });
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(3);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("total Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("http://{address}/total"))
                    .total_timeout(Duration::from_millis(100))
                    .build()
                    .expect("total request must build"),
            )
            .expect("total request must submit");
        reader.wait_head().expect("total head must arrive");
        thread::sleep(Duration::from_millis(200));
        let Err(crate::StreamError::Failed(error)) = reader.try_read(&mut [0_u8; 1]) else {
            panic!("total timeout must fail a backpressured stream")
        };
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(error.timeout_kind(), Some(TimeoutKind::Total));
        engine.shutdown().expect("total Engine must stop");
        server.join().expect("total fixture must join");
    }

    #[test]
    fn aggregate_stream_pressure_releases_windows_after_cancel_and_drain() {
        const INITIAL: usize = 8;
        const REPLACEMENTS: usize = 2;
        const WINDOW: usize = 64;
        const BODY: usize = 4096;

        let listener = TcpListener::bind("127.0.0.1:0").expect("stream pressure must bind");
        let address = listener.local_addr().expect("stream pressure address");
        let server = thread::spawn(move || {
            let mut handlers = Vec::with_capacity(INITIAL + REPLACEMENTS);
            for _ in 0..INITIAL + REPLACEMENTS {
                let (mut stream, _) = listener.accept().expect("stream pressure must accept");
                handlers.push(thread::spawn(move || {
                    read_request_head(&mut stream, "stream pressure request head");
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {BODY}\r\nConnection: close\r\n\r\n"
                    );
                    stream
                        .write_all(head.as_bytes())
                        .expect("stream pressure head must write");
                    stream
                        .write_all(&vec![b'x'; BODY])
                        .expect("stream pressure body must write");
                }));
            }
            for handler in handlers {
                handler.join().expect("stream pressure handler must join");
            }
        });

        let config = EngineConfig::spawned()
            .with_max_connections(
                std::num::NonZeroUsize::new(INITIAL).expect("initial count is non-zero"),
            )
            .with_max_connections_per_origin(
                std::num::NonZeroUsize::new(INITIAL).expect("initial count is non-zero"),
            )
            .with_max_idle_connections(0)
            .with_max_idle_connections_per_origin(0)
            .with_max_stream_queue_bytes_per_request(WINDOW)
            .with_max_stream_queued_bytes(INITIAL * WINDOW);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("stream pressure Engine must construct");
        let client = engine.client();
        let mut readers = Vec::with_capacity(INITIAL);
        for index in 0..INITIAL {
            readers.push(
                client
                    .submit_stream(
                        StreamRequest::get(format!("http://{address}/initial-{index}"))
                            .total_timeout(Duration::from_secs(5))
                            .build()
                            .expect("stream pressure request must build"),
                    )
                    .expect("stream pressure request must submit"),
            );
        }
        for reader in &mut readers {
            assert_eq!(
                reader
                    .wait_head()
                    .expect("every stream pressure head must arrive")
                    .status(),
                200
            );
        }
        let fill_deadline = Instant::now() + Duration::from_secs(2);
        while readers
            .iter()
            .any(|reader| reader.queued_bytes_for_test() != WINDOW)
        {
            assert!(
                Instant::now() < fill_deadline,
                "every response window must reach its exact bounded full state"
            );
            thread::yield_now();
        }
        assert_eq!(
            engine.metrics().current().reserved_stream_queue_bytes(),
            INITIAL * WINDOW
        );

        for index in [0, INITIAL / 2] {
            readers[index]
                .handle()
                .cancel()
                .expect("selected full stream must cancel");
        }
        for index in [0, INITIAL / 2] {
            assert!(matches!(
                readers[index].try_read(&mut [0_u8; 1]),
                Err(crate::StreamError::Cancelled)
            ));
        }

        let mut replacements = Vec::with_capacity(REPLACEMENTS);
        for index in 0..REPLACEMENTS {
            replacements.push(
                client
                    .submit_stream(
                        StreamRequest::get(format!("http://{address}/replacement-{index}"))
                            .total_timeout(Duration::from_secs(5))
                            .build()
                            .expect("replacement stream must build"),
                    )
                    .expect("cancelled reservations must admit replacement streams"),
            );
        }

        for (index, reader) in readers.into_iter().enumerate() {
            if index == 0 || index == INITIAL / 2 {
                continue;
            }
            let response = reader
                .collect()
                .expect("surviving full stream must drain to completion");
            assert_eq!(response.body().len(), BODY);
            assert!(response.body().iter().all(|byte| *byte == b'x'));
        }
        for reader in replacements {
            let response = reader
                .collect()
                .expect("replacement stream must drain to completion");
            assert_eq!(response.body().len(), BODY);
        }

        let release_deadline = Instant::now() + Duration::from_secs(1);
        while engine.metrics().current().reserved_stream_queue_bytes() != 0 {
            assert!(
                Instant::now() < release_deadline,
                "terminal stream reservations must be reaped promptly"
            );
            thread::yield_now();
        }
        let metrics = engine.metrics();
        assert_eq!(metrics.requests_accepted(), (INITIAL + REPLACEMENTS) as u64);
        assert_eq!(metrics.requests_completed(), INITIAL as u64);
        assert_eq!(metrics.requests_cancelled(), REPLACEMENTS as u64);
        assert_eq!(metrics.requests_failed(), 0);
        assert_eq!(metrics.current().reserved_stream_queue_bytes(), 0);
        assert_eq!(
            metrics.high_water().reserved_stream_queue_bytes(),
            INITIAL * WINDOW
        );
        assert_eq!(metrics.current().active_connections(), 0);
        engine.shutdown().expect("stream pressure Engine must stop");
        server.join().expect("stream pressure fixture must join");
    }

    #[test]
    fn buffered_stream_request_discards_redirect_body_and_publishes_only_final_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stream redirect must bind");
        let address = listener.local_addr().expect("stream redirect address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("stream redirect must accept");
            read_request_head(&mut stream, "stream redirect first request");
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nContent-Length: 3\r\nLocation: /final\r\n\r\nold",
                )
                .expect("stream redirect response must write");
            read_request_head(&mut stream, "stream redirect final request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew")
                .expect("stream redirect final response must write");
        });
        let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(2);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("stream redirect Engine must construct");
        let reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("http://{address}/first"))
                    .redirect_limit(2)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("stream redirect request must build"),
            )
            .expect("stream redirect request must submit");
        let response = reader.collect().expect("stream redirect must complete");
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"new");
        engine.shutdown().expect("stream redirect Engine must stop");
        server.join().expect("stream redirect fixture must join");
    }

    #[test]
    fn public_stream_no_body_is_immediate_eof_and_decode_error_keeps_its_stage() {
        for malformed in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("stream rule fixture must bind");
            let address = listener.local_addr().expect("stream rule fixture address");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("stream rule fixture must accept");
                read_request_head(&mut stream, "stream rule request");
                let response = if malformed {
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZ\r\n".as_slice()
                } else {
                    b"HTTP/1.1 204 No Content\r\n\r\n".as_slice()
                };
                stream
                    .write_all(response)
                    .expect("stream rule response must write");
            });
            let config = EngineConfig::spawned();
            let engine = Engine::with_spawned_factory(
                config.clone(),
                Box::new(NativeHttpFactory::new(&config)),
            )
            .expect("stream rule Engine must construct");
            let mut reader = engine
                .client()
                .submit_stream(
                    StreamRequest::get(format!("http://{address}/rule"))
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("stream rule request must build"),
                )
                .expect("stream rule request must submit");
            if malformed {
                assert_eq!(
                    reader
                        .wait_head()
                        .expect("malformed response head must publish")
                        .status(),
                    200
                );
                let Err(crate::StreamError::Failed(error)) = reader.read(&mut [0_u8; 1]) else {
                    panic!("malformed streamed body must fail the reader")
                };
                assert_eq!(error.kind(), ErrorKind::Transport);
                assert_eq!(error.transport_stage(), Some(TransportStage::Http));
                assert_eq!(engine.metrics().requests_failed(), 1);
            } else {
                assert_eq!(
                    reader.wait_head().expect("204 head must publish").status(),
                    204
                );
                assert!(reader.is_eof());
                assert_eq!(reader.read(&mut [0_u8; 1]).expect("204 must be EOF"), None);
                assert_eq!(engine.metrics().requests_completed(), 1);
            }
            engine.shutdown().expect("stream rule Engine must stop");
            server.join().expect("stream rule fixture must join");
        }
    }

    #[test]
    fn stream_cancel_and_engine_shutdown_close_stalled_sockets_and_readers() {
        for shutdown in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("cancel fixture must bind");
            let address = listener.local_addr().expect("cancel fixture address");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("cancel fixture must accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("cancel fixture timeout must configure");
                read_request_head(&mut stream, "cancel request head");
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc")
                    .expect("cancel partial response must write");
                let mut byte = [0_u8; 1];
                assert_socket_closed(&mut stream, &mut byte, "stream cancellation");
            });
            let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(3);
            let engine = Engine::with_spawned_factory(
                config.clone(),
                Box::new(NativeHttpFactory::new(&config)),
            )
            .expect("cancel Engine must construct");
            let mut reader = engine
                .client()
                .submit_stream(
                    StreamRequest::get(format!("http://{address}/cancel"))
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("cancel request must build"),
                )
                .expect("cancel request must submit");
            reader.wait_head().expect("cancel head must arrive");
            if shutdown {
                engine.shutdown().expect("streaming Engine must shut down");
            } else {
                reader
                    .handle()
                    .cancel()
                    .expect("stream request must cancel");
                engine.shutdown().expect("cancel Engine must stop");
            }
            assert!(matches!(
                reader.try_read(&mut [0_u8; 1]),
                Err(crate::StreamError::Cancelled)
            ));
            server.join().expect("cancel fixture must join");
        }
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
        let backend = NativeHttpBackend::new(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits::from_config(&config),
        )
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
    fn manual_native_stream_reader_progresses_only_when_the_owner_drives() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("manual stream fixture must bind");
        let address = listener
            .local_addr()
            .expect("manual stream fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("manual stream fixture must accept");
            read_request_head(&mut stream, "manual stream request head");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nmanual")
                .expect("manual stream response must write");
        });

        let config = EngineConfig::manual().with_max_stream_queue_bytes_per_request(3);
        let backend = NativeHttpBackend::new(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits::from_config(&config),
        )
        .expect("manual streaming backend must construct");
        let mut engine = Engine::with_backend(config, Box::new(backend))
            .expect("manual streaming Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::get(format!("http://{address}/manual-stream"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("manual stream request must build"),
            )
            .expect("manual stream request must submit");
        assert!(
            reader
                .try_head()
                .expect("passive head probe must work")
                .is_none()
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut received = Vec::new();
        let mut buffer = [0_u8; 2];
        loop {
            engine
                .drive((Instant::now() + Duration::from_millis(10)).min(deadline))
                .expect("manual stream drive must succeed");
            match reader
                .try_read(&mut buffer)
                .expect("manual passive read must succeed")
            {
                crate::StreamRead::Pending => {
                    assert!(Instant::now() < deadline, "manual stream timed out")
                }
                crate::StreamRead::Data(read) => received.extend_from_slice(&buffer[..read]),
                crate::StreamRead::Eof => break,
            }
        }
        assert_eq!(received, b"manual");
        engine
            .shutdown()
            .expect("manual streaming Engine must stop");
        server.join().expect("manual stream fixture must join");
    }

    #[test]
    fn manual_streamed_upload_uses_try_push_and_owner_drive_only() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("manual upload fixture must bind");
        let address = listener.local_addr().expect("manual upload address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("manual upload must accept");
            let request = read_request_wire(&mut stream, "manual streamed upload");
            let head_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("manual upload head delimiter")
                + 4;
            assert_eq!(&request[head_end..], b"data");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                .expect("manual upload response must write");
        });

        let (body, mut sender) = UploadBody::fixed(4, 4).expect("manual upload pair");
        sender
            .try_push(b"dat".to_vec())
            .expect("manual prequeue fits");
        let config = EngineConfig::manual().with_max_stream_queue_bytes_per_request(4);
        let backend = NativeHttpBackend::new(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits::from_config(&config),
        )
        .expect("manual upload backend must construct");
        let mut engine = Engine::with_backend(config, Box::new(backend))
            .expect("manual upload Engine must construct");
        let mut reader = engine
            .client()
            .submit_stream(
                StreamRequest::post(format!("http://{address}/manual-upload"))
                    .body_stream(body)
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("manual upload request must build"),
            )
            .expect("manual upload request must submit");
        let error = sender
            .push(b"a".to_vec())
            .expect_err("manual blocking push must fail");
        assert_eq!(error.kind(), TryPushErrorKind::WrongMode);
        assert_eq!(error.into_chunk(), b"a");
        sender
            .try_push(b"a".to_vec())
            .expect("manual try_push works");
        sender.finish().expect("manual upload must finish");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            engine
                .drive((Instant::now() + Duration::from_millis(10)).min(deadline))
                .expect("manual upload drive must succeed");
            match reader.try_head() {
                Ok(Some(head)) => {
                    assert_eq!(head.status(), 204);
                    assert!(reader.is_eof());
                    break;
                }
                Ok(None) => assert!(Instant::now() < deadline, "manual upload timed out"),
                Err(error) => panic!("manual upload failed: {error}"),
            }
        }
        engine.shutdown().expect("manual upload Engine must stop");
        server.join().expect("manual upload fixture must join");
    }

    #[test]
    fn abandoned_and_length_mismatched_uploads_fail_at_send_stage() {
        for abandon in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("producer failure must bind");
            let address = listener.local_addr().expect("producer failure address");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("producer failure must accept");
                if !peer_observed_close(&mut stream) {
                    assert_socket_closed(&mut stream, &mut [0_u8; 1024], "producer failure");
                }
            });
            let (body, sender) = UploadBody::fixed(4, 4).expect("producer failure pair");
            let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(4);
            let engine = Engine::with_spawned_factory(
                config.clone(),
                Box::new(NativeHttpFactory::new(&config)),
            )
            .expect("producer failure Engine must construct");
            let mut reader = engine
                .client()
                .submit_stream(
                    StreamRequest::post(format!("http://{address}/producer-failure"))
                        .body_stream(body)
                        .total_timeout(Duration::from_secs(2))
                        .build()
                        .expect("producer failure request must build"),
                )
                .expect("producer failure request must submit");
            if abandon {
                drop(sender);
            } else {
                let error = sender
                    .finish()
                    .expect_err("short fixed producer must reject finish");
                assert_eq!(error.kind(), crate::UploadFinishErrorKind::LengthMismatch);
            }
            let Err(crate::StreamError::Failed(error)) = reader.wait_head() else {
                panic!("producer failure must fail the reader")
            };
            assert_eq!(error.kind(), ErrorKind::Transport);
            assert_eq!(error.transport_stage(), Some(TransportStage::Send));
            engine
                .shutdown()
                .expect("producer failure Engine must stop");
            server.join().expect("producer failure fixture must join");
        }
    }

    #[test]
    fn cancel_and_shutdown_wake_blocked_upload_producers() {
        for shutdown in [false, true] {
            const BODY_BYTES: usize = 8 * 1024 * 1024;
            let listener = TcpListener::bind("127.0.0.1:0").expect("upload stop fixture must bind");
            let address = listener.local_addr().expect("upload stop address");
            let (head_tx, head_rx) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("upload stop must accept");
                read_request_head(&mut stream, "upload stop request head");
                head_tx.send(()).expect("upload stop head must signal");
                // Cancellation closes the socket promptly, but bytes already accepted by the
                // kernel before the terminal winner may still reach the peer first. Drain those
                // bounded in-flight bytes and require the close rather than pretending cancel can
                // recall a completed socket write.
                drain_until_socket_closed(&mut stream, "upload stop");
            });
            let (body, mut sender) =
                UploadBody::fixed(BODY_BYTES as u64, 1024).expect("upload stop pair");
            let config = EngineConfig::spawned().with_max_stream_queue_bytes_per_request(1024);
            let engine = Engine::with_spawned_factory(
                config.clone(),
                Box::new(NativeHttpFactory::new(&config)),
            )
            .expect("upload stop Engine must construct");
            let mut reader = engine
                .client()
                .submit_stream(
                    StreamRequest::post(format!("http://{address}/upload-stop"))
                        .body_stream(body)
                        .total_timeout(Duration::from_secs(3))
                        .build()
                        .expect("upload stop request must build"),
                )
                .expect("upload stop request must submit");
            let producer = thread::spawn(move || sender.push(vec![b'x'; BODY_BYTES]));
            head_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("upload stop server must observe head");
            if shutdown {
                engine
                    .shutdown()
                    .expect("upload Engine shutdown must complete");
            } else {
                reader
                    .handle()
                    .cancel()
                    .expect("upload cancellation must win");
                engine
                    .shutdown()
                    .expect("cancelled upload Engine must stop");
            }
            let error = producer
                .join()
                .expect("blocked upload producer must join")
                .expect_err("blocked upload producer must wake closed");
            assert_eq!(error.kind(), TryPushErrorKind::Closed);
            assert!(!error.into_chunk().is_empty());
            assert!(matches!(
                reader.try_head(),
                Err(crate::StreamError::Cancelled)
            ));
            server.join().expect("upload stop fixture must join");
        }
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
    fn capped_connection_pressure_survives_mixed_peer_interruptions() {
        const BATCH: usize = 64;
        const STALLED: usize = BATCH / 8;
        const INTERRUPTED: usize = BATCH / 8;

        let listener = TcpListener::bind("127.0.0.1:0").expect("pressure fixture must bind");
        let address = listener.local_addr().expect("pressure fixture address");
        let (stalled_tx, stalled_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut handlers = Vec::with_capacity(BATCH + 1);
            for _ in 0..=BATCH {
                let (mut stream, _) = listener.accept().expect("pressure fixture must accept");
                let stalled_tx = stalled_tx.clone();
                handlers.push(thread::spawn(move || {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .expect("pressure socket timeout must configure");
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 512];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let read = stream.read(&mut buffer).expect("pressure request must read");
                        assert_ne!(read, 0, "pressure client closed before its request head");
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let first_line_end = request
                        .windows(2)
                        .position(|window| window == b"\r\n")
                        .expect("pressure request must have a first line");
                    let first_line = std::str::from_utf8(&request[..first_line_end])
                        .expect("pressure request line must be UTF-8");
                    let path = first_line
                        .split_ascii_whitespace()
                        .nth(1)
                        .expect("pressure request must have a target");
                    if path == "/health" {
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                            )
                            .expect("health response must write");
                        return;
                    }
                    let index = path
                        .strip_prefix('/')
                        .expect("pressure target must be origin-form")
                        .parse::<usize>()
                        .expect("pressure target must contain its index");
                    match index % 8 {
                        0 => {
                            stalled_tx
                                .send(index)
                                .expect("stalled pressure request must signal");
                            assert_socket_closed(
                                &mut stream,
                                &mut buffer,
                                "cancelled pressure request",
                            );
                        }
                        1 => {
                            stream
                                .shutdown(std::net::Shutdown::Both)
                                .expect("interrupted pressure socket must close");
                        }
                        _ => {
                            stream
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                                )
                                .expect("pressure response must write");
                        }
                    }
                }));
            }
            for handler in handlers {
                handler
                    .join()
                    .expect("pressure connection handler must join");
            }
        });

        let config = EngineConfig::spawned()
            .with_max_connections(std::num::NonZeroUsize::new(4).expect("four is non-zero"))
            .with_max_connections_per_origin(
                std::num::NonZeroUsize::new(4).expect("four is non-zero"),
            )
            .with_max_idle_connections(0)
            .with_max_idle_connections_per_origin(0);
        let engine =
            Engine::with_spawned_factory(config.clone(), Box::new(NativeHttpFactory::new(&config)))
                .expect("pressure Engine must construct");
        let client = engine.client();
        let mut pending = Vec::with_capacity(BATCH);
        let mut handles = Vec::with_capacity(BATCH);
        for index in 0..BATCH {
            let request = client
                .submit(
                    Request::get(format!("http://{address}/{index}"))
                        .total_timeout(Duration::from_secs(5))
                        .build()
                        .expect("pressure request must build"),
                )
                .expect("pressure request must submit");
            handles.push(request.handle());
            pending.push(request);
        }
        for _ in 0..STALLED {
            let index = stalled_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("every stalled request must reach the peer");
            handles[index]
                .cancel()
                .expect("stalled pressure request must cancel");
        }

        let mut completed = 0;
        let mut failed = 0;
        let mut cancelled = 0;
        for request in pending {
            match request.wait() {
                Completion::Completed(response) => {
                    assert_eq!(response.status(), 200);
                    assert_eq!(response.body(), b"ok");
                    completed += 1;
                }
                Completion::Failed(error) => {
                    assert_eq!(error.kind(), ErrorKind::Transport);
                    assert_eq!(error.transport_stage(), Some(TransportStage::Receive));
                    failed += 1;
                }
                Completion::Cancelled => cancelled += 1,
            }
        }
        assert_eq!(completed, BATCH - STALLED - INTERRUPTED);
        assert_eq!(failed, INTERRUPTED);
        assert_eq!(cancelled, STALLED);

        let health = client
            .execute(
                Request::get(format!("http://{address}/health"))
                    .total_timeout(Duration::from_secs(2))
                    .build()
                    .expect("health request must build"),
            )
            .expect("owner must remain healthy after mixed interruptions");
        assert_eq!(health.body(), b"ok");

        let metrics = engine.metrics();
        assert_eq!(metrics.requests_accepted(), (BATCH + 1) as u64);
        assert_eq!(metrics.requests_completed(), (completed + 1) as u64);
        assert_eq!(metrics.requests_failed(), failed as u64);
        assert_eq!(metrics.requests_cancelled(), cancelled as u64);
        assert_eq!(metrics.current().active_connections(), 0);
        assert_eq!(metrics.current().connection_waiters(), 0);
        assert_eq!(metrics.high_water().active_connections(), 4);
        assert!(metrics.high_water().connection_waiters() > 0);
        assert_eq!(metrics.connections_opened(), (BATCH + 1) as u64);
        assert_eq!(metrics.connections_closed(), (BATCH + 1) as u64);
        engine.shutdown().expect("pressure Engine must stop");
        server.join().expect("pressure fixture must join");
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
