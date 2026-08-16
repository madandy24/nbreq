//! Private libcurl Multi proving backend.
//!
//! No curl type crosses the backend boundary. `Multi` and every easy handle are constructed,
//! driven, cancelled, and dropped on the spawned reactor thread. Only curl's thread-safe weak
//! `MultiWaker` is installed in the Engine command queue.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use curl::easy::{Easy2, Handler, HttpVersion, List, WriteError};
use curl::multi::{Easy2Handle, Multi};

use super::{Backend, BackendCompletion, BackendFactory, PollMode};
use crate::registry::Shared;
use crate::{
    Completion, Error, ErrorKind, Header, Method, Request, RequestId, Response, ShutdownError,
    TlsVerification,
};

pub(super) struct CurlFactory;

impl CurlFactory {
    pub(super) fn new() -> Self {
        Self
    }
}

impl BackendFactory for CurlFactory {
    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error> {
        // The pinned binding's NBReq patch disables its loader-sensitive CRT constructor. This is
        // therefore the sole initialization path, on an ordinary spawned reactor thread.
        curl::init();
        let multi = Multi::new();
        let waker = multi.waker();
        shared.queue.set_external_waker(Some(Arc::new(move || {
            let _wake_result = waker.wakeup();
        })));
        Ok(Box::new(CurlBackend {
            multi,
            handles: HashMap::new(),
            id_to_token: HashMap::new(),
            token_to_id: HashMap::new(),
            next_token: 1,
            fatal_error: None,
        }))
    }
}

struct CurlBackend {
    multi: Multi,
    handles: HashMap<RequestId, ActiveTransfer>,
    id_to_token: HashMap<RequestId, usize>,
    token_to_id: HashMap<usize, RequestId>,
    next_token: usize,
    fatal_error: Option<Error>,
}

struct ActiveTransfer {
    handle: Easy2Handle<ResponseCollector>,
    request: Request,
    redirect_hops: u8,
    started: Instant,
}

#[derive(Default)]
struct ResponseCollector {
    headers: Vec<Header>,
    body: Vec<u8>,
    header_error: bool,
}

impl Handler for ResponseCollector {
    fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        self.body.extend_from_slice(data);
        Ok(data.len())
    }

    fn header(&mut self, data: &[u8]) -> bool {
        let line = trim_line_end(data);
        if line.starts_with(b"HTTP/") {
            self.headers.clear();
            return true;
        }
        if line.is_empty() {
            return true;
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            self.header_error = true;
            return false;
        };
        let Ok(name) = std::str::from_utf8(&line[..colon]) else {
            self.header_error = true;
            return false;
        };
        if name.is_empty() {
            self.header_error = true;
            return false;
        }
        let value = trim_header_value(&line[colon + 1..]);
        self.headers.push(Header::new(name, value.to_vec()));
        true
    }
}

impl Backend for CurlBackend {
    fn submit(&mut self, id: RequestId, request: Request) -> Option<Completion> {
        self.start_transfer(id, request, 0, Instant::now())
            .err()
            .map(Completion::Failed)
    }

    fn cancel(&mut self, id: RequestId) {
        let Some(active) = self.handles.remove(&id) else {
            return;
        };
        self.remove_token(id);
        if let Err(error) = self.multi.remove2(active.handle) {
            self.fatal_error = Some(multi_error(error));
        }
    }

    fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
        if let Some(error) = self.fatal_error.take() {
            return Err(error);
        }

        self.multi.perform().map_err(multi_error)?;
        let mut completions = self.collect_completions()?;
        if completions.is_empty() && !self.handles.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                self.multi.poll(&mut [], remaining).map_err(multi_error)?;
                self.multi.perform().map_err(multi_error)?;
                completions.extend(self.collect_completions()?);
            }
        }
        Ok(completions)
    }

    fn shutdown(&mut self) -> Result<(), ShutdownError> {
        let handles = self.handles.drain().collect::<Vec<_>>();
        self.id_to_token.clear();
        self.token_to_id.clear();
        let mut failure = self.fatal_error.take();
        for (_id, active) in handles {
            if let Err(error) = self.multi.remove2(active.handle)
                && failure.is_none()
            {
                failure = Some(multi_error(error));
            }
        }
        if let Some(error) = failure {
            return Err(ShutdownError::new(error));
        }
        Ok(())
    }

    fn poll_mode(&self) -> PollMode {
        PollMode::Interruptible
    }
}

impl CurlBackend {
    fn start_transfer(
        &mut self,
        id: RequestId,
        request: Request,
        redirect_hops: u8,
        started: Instant,
    ) -> Result<(), Error> {
        let easy = configured_easy(&request, started)?;
        let token = self.allocate_token()?;
        let mut handle = self.multi.add2(easy).map_err(multi_error)?;
        if let Err(error) = handle.set_token(token) {
            let _remove_result = self.multi.remove2(handle);
            return Err(curl_error(error));
        }
        self.id_to_token.insert(id, token);
        self.token_to_id.insert(token, id);
        self.handles.insert(
            id,
            ActiveTransfer {
                handle,
                request,
                redirect_hops,
                started,
            },
        );
        Ok(())
    }

    fn allocate_token(&mut self) -> Result<usize, Error> {
        let start = self.next_token;
        loop {
            let token = self.next_token;
            self.next_token = self.next_token.wrapping_add(1).max(1);
            if !self.token_to_id.contains_key(&token) {
                return Ok(token);
            }
            if self.next_token == start {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "curl transfer token space is exhausted",
                ));
            }
        }
    }

    fn remove_token(&mut self, id: RequestId) {
        if let Some(token) = self.id_to_token.remove(&id) {
            self.token_to_id.remove(&token);
        }
    }

    fn collect_completions(&mut self) -> Result<Vec<BackendCompletion>, Error> {
        let handles = &self.handles;
        let token_to_id = &self.token_to_id;
        let mut finished = Vec::new();
        self.multi.messages(|message| {
            let Ok(token) = message.token() else {
                return;
            };
            let Some(id) = token_to_id.get(&token).copied() else {
                return;
            };
            let Some(active) = handles.get(&id) else {
                return;
            };
            if let Some(result) = message.result_for2(&active.handle) {
                finished.push((id, result));
            }
        });

        let mut completions = Vec::with_capacity(finished.len());
        for (id, result) in finished {
            let active = self.handles.remove(&id).ok_or_else(|| {
                Error::new(
                    ErrorKind::Internal,
                    "curl completion lost its owned easy handle",
                )
            })?;
            self.remove_token(id);
            let easy = self.multi.remove2(active.handle).map_err(multi_error)?;
            let completion = match result {
                Ok(()) => match redirect_request(&easy, &active.request, active.redirect_hops) {
                    Ok(Some(request)) => {
                        match self.start_transfer(
                            id,
                            request,
                            active.redirect_hops.saturating_add(1),
                            active.started,
                        ) {
                            Ok(()) => continue,
                            Err(error) => Err(error),
                        }
                    }
                    Ok(None) => completed_response(&easy),
                    Err(error) => Err(error),
                },
                Err(_error) if easy.get_ref().header_error => Err(Error::new(
                    ErrorKind::Transport,
                    "curl rejected a malformed response header",
                )),
                Err(error) => Err(curl_error(error)),
            };
            completions.push(BackendCompletion {
                id,
                completion: match completion {
                    Ok(response) => Completion::Completed(response),
                    Err(error) => Completion::Failed(error),
                },
            });
        }
        Ok(completions)
    }
}

fn configured_easy(request: &Request, started: Instant) -> Result<Easy2<ResponseCollector>, Error> {
    validate_request(request)?;
    let mut easy = Easy2::new(ResponseCollector::default());
    easy.url(request.url()).map_err(curl_error)?;
    easy.proxy("").map_err(curl_error)?;
    easy.http_version(HttpVersion::V11).map_err(curl_error)?;
    easy.follow_location(false).map_err(curl_error)?;

    let body = request.body();
    match request.method() {
        Method::Get if body.is_empty() => {}
        Method::Head => easy.nobody(true).map_err(curl_error)?,
        Method::Post => {
            easy.post(true).map_err(curl_error)?;
            easy.post_fields_copy(body).map_err(curl_error)?;
        }
        method => {
            if !body.is_empty() {
                easy.post_fields_copy(body).map_err(curl_error)?;
            }
            easy.custom_request(method_name(method))
                .map_err(curl_error)?;
        }
    }

    let mut headers = List::new();
    let has_expect = request
        .headers()
        .iter()
        .any(|header| header.name().eq_ignore_ascii_case("expect"));
    for header in request.headers() {
        let value = std::str::from_utf8(header.value()).map_err(|_| {
            Error::new(
                ErrorKind::InvalidRequest,
                "curl pilot request header values must be UTF-8",
            )
        })?;
        headers
            .append(&format!("{}: {value}", header.name()))
            .map_err(curl_error)?;
    }
    if !has_expect {
        headers.append("Expect:").map_err(curl_error)?;
    }
    easy.http_headers(headers).map_err(curl_error)?;

    let options = request.options();
    if options.tls_verification == TlsVerification::DangerouslyDisableCertificateVerification {
        easy.ssl_verify_host(false).map_err(curl_error)?;
        easy.ssl_verify_peer(false).map_err(curl_error)?;
    }
    if let Some(timeout) = options.connect_timeout {
        easy.connect_timeout(timeout).map_err(curl_error)?;
    }
    if let Some(timeout) = remaining_total_timeout(options.total_timeout, started)? {
        easy.timeout(timeout).map_err(curl_error)?;
    }
    if let Some(timeout) = options.inactivity_timeout {
        easy.low_speed_limit(1).map_err(curl_error)?;
        easy.low_speed_time(timeout).map_err(curl_error)?;
    }
    Ok(easy)
}

fn validate_request(request: &Request) -> Result<(), Error> {
    http_origin(request.url(), ErrorKind::InvalidRequest)?;
    for header in request.headers() {
        if header.name().is_empty()
            || header
                .name()
                .bytes()
                .any(|byte| byte == b':' || byte <= 0x20 || byte >= 0x7f)
            || header.value().contains(&b'\r')
            || header.value().contains(&b'\n')
        {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "request contains an invalid HTTP header",
            ));
        }
    }
    Ok(())
}

fn remaining_total_timeout(
    configured: Option<Duration>,
    started: Instant,
) -> Result<Option<Duration>, Error> {
    let Some(configured) = configured else {
        return Ok(None);
    };
    let remaining = configured.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(Error::new(
            ErrorKind::Timeout,
            "the total request timeout expired during redirect processing",
        ));
    }
    Ok(Some(remaining))
}

fn redirect_request(
    easy: &Easy2<ResponseCollector>,
    request: &Request,
    redirect_hops: u8,
) -> Result<Option<Request>, Error> {
    let status = easy.response_code().map_err(curl_error)?;
    let (method, keep_body) = match status {
        301 | 302 => match request.method() {
            Method::Get | Method::Head => (request.method().clone(), true),
            _ => return Ok(None),
        },
        303 => match request.method() {
            Method::Head => (Method::Head, false),
            _ => (Method::Get, false),
        },
        307 | 308 => (request.method().clone(), true),
        _ => return Ok(None),
    };

    let limit = request.options().redirect_limit;
    if limit == 0 {
        return Ok(None);
    }
    if redirect_hops >= limit {
        return Err(Error::new(
            ErrorKind::Redirect,
            format!("redirect limit of {limit} hops was exceeded"),
        ));
    }

    let Some(target) = easy.redirect_url().map_err(curl_error)? else {
        return Ok(None);
    };
    let target = target.to_owned();
    let source_origin = http_origin(request.url(), ErrorKind::Redirect)?;
    let target_origin = http_origin(&target, ErrorKind::Redirect)?;
    if source_origin.scheme == "https" && target_origin.scheme == "http" {
        return Err(Error::new(
            ErrorKind::Redirect,
            "an HTTPS-to-HTTP redirect was blocked",
        ));
    }
    let cross_origin = source_origin != target_origin;
    Ok(Some(request.redirected(
        target,
        method,
        keep_body,
        cross_origin,
    )))
}

#[derive(Eq, PartialEq)]
struct HttpOrigin {
    scheme: String,
    host: String,
    port: u16,
}

fn http_origin(url: &str, error_kind: ErrorKind) -> Result<HttpOrigin, Error> {
    let Some((scheme, remainder)) = url.split_once("://") else {
        return Err(Error::new(error_kind, "HTTP URL has no scheme separator"));
    };
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => {
            return Err(Error::new(
                error_kind,
                "NBReq's curl profile permits only HTTP and HTTPS URLs",
            ));
        }
    };
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return Err(Error::new(
            error_kind,
            "HTTP URL authority is empty or contains embedded credentials",
        ));
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some(closing) = bracketed.find(']') else {
            return Err(Error::new(error_kind, "HTTP URL has an invalid IPv6 host"));
        };
        let host = &bracketed[..closing];
        let suffix = &bracketed[closing + 1..];
        let port = if suffix.is_empty() {
            default_port
        } else if let Some(port) = suffix.strip_prefix(':') {
            parse_port(port, error_kind)?
        } else {
            return Err(Error::new(error_kind, "HTTP URL has an invalid authority"));
        };
        (host, port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') {
            return Err(Error::new(
                error_kind,
                "an IPv6 URL host must be enclosed in brackets",
            ));
        }
        (host, parse_port(port, error_kind)?)
    } else {
        (authority, default_port)
    };
    if host.is_empty() {
        return Err(Error::new(error_kind, "HTTP URL host is empty"));
    }
    Ok(HttpOrigin {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
    })
}

fn parse_port(port: &str, error_kind: ErrorKind) -> Result<u16, Error> {
    port.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| Error::new(error_kind, "HTTP URL port is invalid"))
}

fn completed_response(easy: &Easy2<ResponseCollector>) -> Result<Response, Error> {
    let status = easy.response_code().map_err(curl_error)?;
    let status = u16::try_from(status).map_err(|_| {
        Error::new(
            ErrorKind::Transport,
            "curl returned an invalid HTTP status code",
        )
    })?;
    let collector = easy.get_ref();
    Ok(Response::new(
        status,
        collector.headers.clone(),
        collector.body.clone(),
    ))
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

fn trim_line_end(mut line: &[u8]) -> &[u8] {
    while matches!(line.last(), Some(b'\r' | b'\n')) {
        line = &line[..line.len() - 1];
    }
    line
}

fn trim_header_value(mut value: &[u8]) -> &[u8] {
    while matches!(value.first(), Some(b' ' | b'\t')) {
        value = &value[1..];
    }
    while matches!(value.last(), Some(b' ' | b'\t')) {
        value = &value[..value.len() - 1];
    }
    value
}

fn curl_error(error: curl::Error) -> Error {
    let kind = if error.is_operation_timedout() {
        ErrorKind::Timeout
    } else {
        ErrorKind::Transport
    };
    Error::new(
        kind,
        format!("curl transfer failed: {}", error.description()),
    )
}

fn multi_error(error: curl::MultiError) -> Error {
    Error::new(
        ErrorKind::Internal,
        format!("curl Multi operation failed: {}", error.description()),
    )
}
