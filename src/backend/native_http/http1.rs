//! HTTP/1.1 request serialization and incremental response framing.
//!
//! `httparse` recognizes response heads and chunk-size lines. NBReq owns request policy, body
//! framing, limits, EOF semantics, and the state transitions consumed by the native owner.

use super::{HttpLimits, MAX_INFORMATIONAL_RESPONSES};
use crate::stream::{ResponsePushError, ResponseSink, UploadFraming};
use crate::{
    Error, ErrorKind, Header, LimitKind, Method, Request, Response, ResponseHead, TransportStage,
};

const STACK_RESPONSE_HEADER_SLOTS: usize = 32;

#[derive(Debug)]
pub(super) struct SerializedRequest {
    pub(super) bytes: Vec<u8>,
    pub(super) response_to_head: bool,
    pub(super) permits_reuse: bool,
}

#[cfg(test)]
pub(super) fn serialize_request(
    request: &Request,
    limits: HttpLimits,
) -> Result<SerializedRequest, Error> {
    serialize_request_with_upload(request, limits, None)
}

pub(super) fn serialize_request_with_upload(
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
    pub(super) scratch: Vec<u8>,
    status: Option<u16>,
    headers: Vec<Header>,
    pub(super) body: Vec<u8>,
    informational_responses: u8,
    framing_bytes: usize,
    pub(super) permits_reuse: bool,
}

#[derive(Debug)]
pub(super) struct DecodeProgress {
    pub(super) response: Option<Response>,
    pub(super) consumed: usize,
    pub(super) permits_reuse: bool,
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

    pub(super) fn ingest(&mut self, bytes: &[u8]) -> Result<DecodeProgress, Error> {
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
pub(super) struct StreamingResponseDecoder {
    pub(super) limits: HttpLimits,
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
pub(super) enum StreamDecodeProgress {
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
pub(super) struct StreamHeadDecision {
    pub(super) complete: bool,
    pub(super) permits_reuse: bool,
}

#[allow(dead_code)]
impl StreamingResponseDecoder {
    pub(super) fn new(response_to_head: bool, limits: HttpLimits, response: ResponseSink) -> Self {
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

    pub(super) fn ingest(&mut self, bytes: &[u8]) -> Result<StreamDecodeProgress, Error> {
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

    pub(super) fn decide_head(&mut self, deliver: bool) -> Result<StreamHeadDecision, Error> {
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

    pub(super) fn eof(&mut self) -> Result<Option<(bool, bool)>, Error> {
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

    pub(super) fn fail(&mut self, error: Error) {
        if let Some(response) = &mut self.response {
            response.fail(error);
        }
    }

    pub(super) fn socket_capacity(&self) -> usize {
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

    pub(super) fn is_consumer_blocked(&self) -> bool {
        self.output == StreamOutput::Deliver && self.socket_capacity() == 0
    }

    pub(super) fn into_response(mut self) -> Result<ResponseSink, Error> {
        self.take_response()
    }

    pub(super) fn take_response(&mut self) -> Result<ResponseSink, Error> {
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

pub(super) enum ParsedHead {
    Informational,
    Final {
        status: u16,
        headers: Vec<Header>,
        framing: BodyFraming,
        permits_reuse: bool,
    },
}

#[derive(Clone, Copy)]
pub(super) enum BodyFraming {
    None,
    Fixed(usize),
    Chunked,
    CloseDelimited,
}

pub(super) fn parse_response_head(
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
