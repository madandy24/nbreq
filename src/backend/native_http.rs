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
use super::native_dns::{NativeResolver, ResolveKey, ResolverConfig};
use super::native_tls::{
    NativeTls, NativeTlsConfigs, TlsProgress, encrypted_outbound_limit, encrypted_receive_limit,
};
use super::{Backend, BackendCompletion, BackendFactory, PollMode};
use crate::registry::Shared;
use crate::types::{http_origin, redirected_request};
use crate::{
    Completion, EngineConfig, Error, ErrorKind, Header, LimitKind, Method, Request, RequestId,
    Response, ShutdownError, TimeoutKind, TransportStage,
};

const MAX_INFORMATIONAL_RESPONSES: u8 = 8;
const MAX_IDLE_CONNECTIONS: usize = 32;
const MAX_IDLE_CONNECTIONS_PER_ORIGIN: usize = 4;
const IDLE_CONNECTION_LIFETIME: Duration = Duration::from_secs(30);
const MAX_NATIVE_CONNECTIONS: usize = 32;
const MAX_NATIVE_CONNECTIONS_PER_ORIGIN: usize = 8;

#[derive(Clone, Copy)]
struct ConnectionLimits {
    global: usize,
    per_origin: usize,
}

impl ConnectionLimits {
    const DEFAULT: Self = Self {
        global: MAX_NATIVE_CONNECTIONS,
        per_origin: MAX_NATIVE_CONNECTIONS_PER_ORIGIN,
    };
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

pub(super) fn serialize_request(
    request: &Request,
    limits: HttpLimits,
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
    if content_length.is_some_and(|length| length != request.body().len()) {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "request Content-Length does not match the buffered body",
        ));
    }

    let generated_host = host_count == 0;
    let generated_length = content_length.is_none() && !request.body().is_empty();
    let generated_count = usize::from(generated_host) + usize::from(generated_length);
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
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(request.body());
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
    let permits_reuse = response_permits_reuse(version, &headers, framing)?;
    Ok(ParsedHead::Final {
        status,
        headers,
        framing,
        permits_reuse,
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
    let mut location = None;
    for header in response
        .headers()
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

/// Private cleartext factory used to prove HTTP framing over the accepted reactor. Ordinary
/// `Engine::new` does not select it while DNS, TLS, redirects, and the remaining parity gates are
/// incomplete.
#[allow(dead_code)]
pub(super) struct NativeHttpFactory {
    limits: HttpLimits,
    resolver: Option<ResolverConfig>,
    tls: Option<NativeTlsConfigs>,
}

#[allow(dead_code)]
impl NativeHttpFactory {
    pub(super) fn new(config: &EngineConfig) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            resolver: None,
            tls: None,
        }
    }

    pub(super) fn new_with_nameserver(config: &EngineConfig, nameserver: SocketAddr) -> Self {
        Self {
            limits: HttpLimits::from_config(config),
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: None,
        }
    }

    pub(super) fn new_with_system_dns(config: &EngineConfig) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
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
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: Some(NativeTlsConfigs::platform()?),
        })
    }

    pub(super) fn new_with_system_dns_and_platform_tls(
        config: &EngineConfig,
    ) -> Result<Self, Error> {
        Ok(Self {
            limits: HttpLimits::from_config(config),
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
            resolver: Some(ResolverConfig::injected(nameserver)),
            tls: Some(NativeTlsConfigs::with_test_root(root_der.into())?),
        })
    }

    pub(super) fn into_backend(self) -> Result<Box<dyn Backend + Send>, Error> {
        Ok(Box::new(NativeHttpBackend::new(
            self.limits,
            self.resolver,
            self.tls,
        )?))
    }
}

impl BackendFactory for NativeHttpFactory {
    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error> {
        let backend = NativeHttpBackend::new(self.limits, self.resolver, self.tls)?;
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
    tls: Option<NativeTls>,
    connect_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    inactivity_timeout: Option<Duration>,
    inactivity_deadline: Option<Instant>,
    key: ConnectionKey,
    request_permits_reuse: bool,
    request_write_drained: bool,
    request: Request,
    redirect_hops: u8,
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
    tls: Option<NativeTlsConfigs>,
    limits: HttpLimits,
    request_to_slot: HashMap<RequestId, SlotId>,
    transfers: HashMap<SlotId, HttpTransfer>,
    request_to_resolve: HashMap<RequestId, ResolveKey>,
    resolves: HashMap<ResolveKey, PendingResolve>,
    next_resolve_key: u64,
    idle: HashMap<ConnectionKey, VecDeque<IdleConnection>>,
    idle_slots: HashMap<SlotId, ConnectionKey>,
    idle_count: usize,
    connection_count: usize,
    connections_per_key: HashMap<ConnectionKey, usize>,
    waiting: VecDeque<PendingResolve>,
    connection_limits: ConnectionLimits,
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
}

#[derive(Clone, Copy)]
struct PendingDeadlines {
    connect: Option<Instant>,
    total: Option<Instant>,
    inactivity: Option<Instant>,
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
    fn new(
        limits: HttpLimits,
        resolver: Option<ResolverConfig>,
        tls: Option<NativeTlsConfigs>,
    ) -> Result<Self, Error> {
        Self::new_with_connection_limits(limits, resolver, tls, ConnectionLimits::DEFAULT)
    }

    fn new_with_connection_limits(
        limits: HttpLimits,
        resolver: Option<ResolverConfig>,
        tls: Option<NativeTlsConfigs>,
        connection_limits: ConnectionLimits,
    ) -> Result<Self, Error> {
        if connection_limits.global == 0
            || connection_limits.per_origin == 0
            || connection_limits.per_origin > connection_limits.global
        {
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
            next_resolve_key: 1,
            idle: HashMap::new(),
            idle_slots: HashMap::new(),
            idle_count: 0,
            connection_count: 0,
            connections_per_key: HashMap::new(),
            waiting: VecDeque::new(),
            connection_limits,
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
            completions.push(BackendCompletion {
                id: pending.request_id,
                completion: Completion::Failed(Error::timeout(
                    timeout,
                    native_timeout_message(timeout),
                )),
            });
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
        completions
    }

    fn make_pending(
        &self,
        request_id: RequestId,
        request: Request,
        deadlines: PendingDeadlines,
        redirect_hops: u8,
        origin_error_kind: ErrorKind,
    ) -> Result<PendingResolve, Error> {
        let serialized = serialize_request(&request, self.limits)?;
        let origin = http_origin(request.url(), origin_error_kind)?;
        let tls_verification = request.options().tls_verification;
        let inactivity_timeout = request.options().inactivity_timeout;
        let body_bearing = !request.body().is_empty();
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
        })
    }

    fn start_pending(&mut self, pending: PendingResolve) -> Option<Completion> {
        let now = Instant::now();
        if pending
            .next_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            let timeout = pending.expired_timeout(now);
            return Some(Completion::Failed(Error::timeout(
                timeout,
                native_timeout_message(timeout),
            )));
        }
        let pending = self.try_begin_reused(pending)?;
        if !self.make_connection_capacity(&pending.key) || !self.reserve_connection(&pending.key) {
            self.waiting.push_back(pending);
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
        if let Some(transfer) = self.transfers.remove(&slot) {
            self.request_to_slot.remove(&transfer.request_id);
            self.release_connection(&transfer.key);
            completions.push(BackendCompletion {
                id: transfer.request_id,
                completion,
            });
        }
    }

    fn take_idle(&mut self, key: &ConnectionKey) -> Option<IdleConnection> {
        let connection = self.idle.get_mut(key)?.pop_front()?;
        if self.idle.get(key).is_some_and(VecDeque::is_empty) {
            self.idle.remove(key);
        }
        self.idle_slots.remove(&connection.slot);
        self.idle_count = self.idle_count.saturating_sub(1);
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
            pending.serialized.bytes.len()
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
        self.request_to_slot.insert(request_id, idle.slot);
        self.transfers.insert(
            idle.slot,
            HttpTransfer {
                request_id,
                decoder: ResponseDecoder::new(pending.serialized.response_to_head, self.limits),
                body_bearing: pending.body_bearing,
                response_started: false,
                connected: true,
                tls: idle.tls,
                connect_deadline: pending.connect_deadline,
                total_deadline: pending.total_deadline,
                inactivity_timeout: pending.inactivity_timeout,
                inactivity_deadline: pending.inactivity_deadline,
                key: pending.key,
                request_permits_reuse: pending.serialized.permits_reuse,
                request_write_drained: false,
                request: pending.request,
                redirect_hops: pending.redirect_hops,
            },
        );
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
            && self.idle_count < MAX_IDLE_CONNECTIONS
            && per_origin_idle < MAX_IDLE_CONNECTIONS_PER_ORIGIN;
        if reusable {
            let parked =
                Instant::now()
                    .checked_add(IDLE_CONNECTION_LIFETIME)
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
                ) {
                    Ok(pending) => {
                        if let Some(completion) = self.start_pending(pending) {
                            completions.push(BackendCompletion {
                                id: request_id,
                                completion,
                            });
                        }
                    }
                    Err(error) => completions.push(BackendCompletion {
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
                return Some(Completion::Failed(Error::new(
                    ErrorKind::Unsupported,
                    "the selected native proving Engine has no TLS configuration",
                )));
            };
            match configs.connection(
                &pending.host,
                pending.tls_verification,
                pending.serialized.bytes.clone(),
            ) {
                Ok(tls) => Some(tls),
                Err(error) => {
                    self.release_connection(&pending.key);
                    return Some(Completion::Failed(error));
                }
            }
        } else {
            None
        };
        let outbound_limit = if tls.is_some() {
            encrypted_outbound_limit()
        } else {
            pending.serialized.bytes.len()
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
                return Some(Completion::Failed(native_transport_error(failure)));
            }
        };
        if tls.is_none() {
            if let Err(failure) = self.reactor.queue_write(slot, &pending.serialized.bytes) {
                self.reactor.cancel(slot);
                self.release_connection(&pending.key);
                return Some(Completion::Failed(native_transport_error(failure)));
            }
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
                tls,
                connect_deadline: pending.connect_deadline,
                total_deadline: pending.total_deadline,
                inactivity_timeout: pending.inactivity_timeout,
                inactivity_deadline: pending.inactivity_deadline,
                key: pending.key,
                request_permits_reuse: pending.serialized.permits_reuse,
                request_write_drained: false,
                request: pending.request,
                redirect_hops: pending.redirect_hops,
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
                        self.release_connection(&pending.key);
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
                Err(failure) => {
                    self.release_connection(&pending.key);
                    completions.push(BackendCompletion {
                        id: pending.request_id,
                        completion: Completion::Failed(Error::transport(
                            TransportStage::Dns,
                            failure.message,
                        )),
                    });
                }
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
            self.release_connection(&pending.key);
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

    fn start_reserved(&mut self, pending: PendingResolve) -> Option<Completion> {
        if let Ok(ip) = pending.host.parse::<IpAddr>() {
            return self.begin_connection(SocketAddr::new(ip, pending.port), pending);
        }
        let key = match self.next_resolve_key() {
            Ok(key) => key,
            Err(error) => {
                self.release_connection(&pending.key);
                return Some(Completion::Failed(error));
            }
        };
        let Some(resolver) = &self.resolver else {
            self.release_connection(&pending.key);
            return Some(Completion::Failed(Error::new(
                ErrorKind::Unsupported,
                "the native HTTP proving Engine requires an injected resolver for hostnames",
            )));
        };
        if let Err(error) = resolver.resolve(key, pending.host.clone()) {
            self.release_connection(&pending.key);
            return Some(Completion::Failed(error));
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
            Error::transport(
                TransportStage::Tls,
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
                            Completion::Failed(Error::transport(
                                TransportStage::Tls,
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

    fn handle_data(
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
                    Completion::Failed(Error::transport(stage, failure.message)),
                    completions,
                );
                return Ok(());
            }
        }
        if arm_deadline && established && self.transfers.contains_key(&slot) {
            self.pump_tls_request(slot, completions)?;
        }
        let decoded = self.transfers.get_mut(&slot).map(|transfer| {
            transfer.response_started |= !plaintext.is_empty();
            transfer.decoder.ingest(&plaintext)
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
                let eof = self
                    .transfers
                    .get_mut(&slot)
                    .map(|transfer| transfer.decoder.eof());
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
                | NativeEvent::WriteProgress(slot)
                | NativeEvent::WriteDrained(slot)
                | NativeEvent::Data(slot, _)
                | NativeEvent::PeerClosed(slot)
                | NativeEvent::Failed(slot, _)
                | NativeEvent::DeadlineExpired(slot) => *slot,
            };
            if self.idle_slots.contains_key(&event_slot) {
                match event {
                    NativeEvent::WriteProgress(_) | NativeEvent::WriteDrained(_) => {}
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
                    self.handle_connected(slot, !failed_slots.contains(&slot), &mut completions)?
                }
                NativeEvent::WriteProgress(slot) => {
                    self.note_progress(slot, false, !failed_slots.contains(&slot))?;
                    if !failed_slots.contains(&slot) {
                        self.pump_tls_request(slot, &mut completions)?;
                    }
                }
                NativeEvent::WriteDrained(slot) => {
                    self.note_progress(slot, false, !failed_slots.contains(&slot))?;
                    let queued_more = if failed_slots.contains(&slot) {
                        false
                    } else {
                        self.pump_tls_request(slot, &mut completions)?
                    };
                    if !queued_more {
                        if let Some(transfer) = self.transfers.get_mut(&slot) {
                            let encrypted = transfer
                                .tls
                                .as_ref()
                                .is_none_or(NativeTls::request_fully_encrypted);
                            transfer.request_write_drained |= transfer.connected && encrypted;
                        }
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
                            Completion::Failed(Error::transport(
                                TransportStage::Tls,
                                "the peer closed during the TLS handshake",
                            )),
                            &mut completions,
                        );
                        continue;
                    }
                    if tls_state == Some(false) {
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
        ) {
            Ok(pending) => pending,
            Err(error) => return Some(Completion::Failed(error)),
        };
        self.start_pending(pending)
    }

    fn cancel(&mut self, id: RequestId) {
        if let Some(index) = self
            .waiting
            .iter()
            .position(|pending| pending.request_id == id)
        {
            self.waiting.remove(index);
        }
        if let Some(key) = self.request_to_resolve.remove(&id) {
            if let Some(pending) = self.resolves.remove(&key) {
                self.release_connection(&pending.key);
            }
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
        self.expire_idle(Instant::now());
        let mut completions = self.dispatch_waiting();
        completions.extend(self.process_resolver_results()?);
        completions.extend(self.expire_resolves()?);
        completions.extend(self.dispatch_waiting());
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
        completions.extend(self.dispatch_waiting());
        Ok(completions)
    }

    fn shutdown(&mut self) -> Result<(), ShutdownError> {
        self.request_to_slot.clear();
        self.transfers.clear();
        self.request_to_resolve.clear();
        self.resolves.clear();
        self.idle.clear();
        self.idle_slots.clear();
        self.idle_count = 0;
        self.connection_count = 0;
        self.connections_per_key.clear();
        self.waiting.clear();
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
        let mut backend =
            NativeHttpBackend::new(LIMITS, None, None).expect("native backend must construct");
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
                decoder: ResponseDecoder::new(false, LIMITS),
                body_bearing: true,
                response_started: false,
                connected: true,
                tls: None,
                connect_deadline: None,
                total_deadline: Some(deadline),
                inactivity_timeout: Some(Duration::from_secs(1)),
                inactivity_deadline: Some(deadline),
                key: connection_key,
                request_permits_reuse: true,
                request_write_drained: false,
                request: Request::get(format!("http://{address}/batch"))
                    .build()
                    .expect("batch request must build"),
                redirect_hops: 0,
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
        engine.shutdown().expect("reuse Engine must stop");
        server.join().expect("reuse fixture must join");
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
        let backend = NativeHttpBackend::new(HttpLimits::from_config(&config), None, None)
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
        let mut backend =
            NativeHttpBackend::new(LIMITS, None, None).expect("idle expiry backend must construct");
        let expires_at = Instant::now() + IDLE_CONNECTION_LIFETIME;
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
        let backend = NativeHttpBackend::new(HttpLimits::from_config(&config), None, None)
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

        let config = EngineConfig::manual();
        let backend = NativeHttpBackend::new_with_connection_limits(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits {
                global: 2,
                per_origin: 1,
            },
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
        let config = EngineConfig::manual();
        let backend = NativeHttpBackend::new_with_connection_limits(
            HttpLimits::from_config(&config),
            None,
            None,
            ConnectionLimits {
                global: 1,
                per_origin: 1,
            },
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
        let backend = NativeHttpBackend::new(HttpLimits::from_config(&config), None, None)
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
