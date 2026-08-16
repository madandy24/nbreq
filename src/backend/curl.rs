//! Private libcurl Multi proving backend.
//!
//! No curl type crosses the backend boundary. `Multi` and every easy handle are constructed,
//! driven, cancelled, and dropped on the spawned reactor thread. Only curl's thread-safe weak
//! `MultiWaker` is installed in the Engine command queue.

use std::collections::HashMap;
#[cfg(test)]
use std::fs::OpenOptions;
#[cfg(test)]
use std::io::{ErrorKind as IoErrorKind, Write};
#[cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use curl::easy::{Easy2, Handler, HttpVersion, List, WriteError};
use curl::multi::{Easy2Handle, Multi};

use super::{Backend, BackendCompletion, BackendFactory, PollMode, ResponseLimits};
use crate::registry::Shared;
use crate::types::http_origin;
use crate::{
    Completion, Error, ErrorKind, Header, LimitKind, Method, Request, RequestId, Response,
    ShutdownError, TimeoutKind, TlsVerification, TransportStage,
};

pub(super) struct CurlFactory {
    limits: ResponseLimits,
    #[cfg(test)]
    test_ca_pem: Option<Vec<u8>>,
}

impl CurlFactory {
    pub(super) fn new(limits: ResponseLimits) -> Self {
        Self {
            limits,
            #[cfg(test)]
            test_ca_pem: None,
        }
    }

    #[cfg(test)]
    pub(super) fn new_with_test_ca(limits: ResponseLimits, ca_pem: Vec<u8>) -> Self {
        Self {
            limits,
            test_ca_pem: Some(ca_pem),
        }
    }
}

impl BackendFactory for CurlFactory {
    fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error> {
        // The pinned binding's NBReq patch disables its loader-sensitive CRT constructor. This is
        // therefore the sole initialization path, on an ordinary spawned reactor thread.
        curl::try_init().map_err(|error| {
            Error::new(
                ErrorKind::Internal,
                format!("failed to initialize libcurl: {}", error.description()),
            )
        })?;
        #[cfg(test)]
        let test_ca = self.test_ca_pem.map(TestCa::new).transpose()?;
        let multi = Multi::new();
        let waker = multi.waker();
        shared
            .queue
            .set_external_waker(Some(Arc::new(move || waker.wakeup().map_err(multi_error))));
        Ok(Box::new(CurlBackend {
            multi,
            limits: self.limits,
            #[cfg(test)]
            test_ca,
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
    limits: ResponseLimits,
    #[cfg(test)]
    test_ca: Option<TestCa>,
    handles: HashMap<RequestId, ActiveTransfer>,
    id_to_token: HashMap<RequestId, usize>,
    token_to_id: HashMap<usize, RequestId>,
    next_token: usize,
    fatal_error: Option<Error>,
}

#[cfg(test)]
enum TestCa {
    Blob(Vec<u8>),
    File(TestCaFile),
}

#[cfg(test)]
impl TestCa {
    fn new(pem: Vec<u8>) -> Result<Self, Error> {
        const CAINFO_BLOB_MINIMUM: u32 = 0x07_4d_00;
        if curl::Version::get().version_num() >= CAINFO_BLOB_MINIMUM {
            Ok(Self::Blob(pem))
        } else {
            TestCaFile::new(&pem).map(Self::File)
        }
    }
}

#[cfg(test)]
struct TestCaFile {
    path: PathBuf,
}

#[cfg(test)]
impl TestCaFile {
    fn new(pem: &[u8]) -> Result<Self, Error> {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nbreq-test-ca-{}-{sequence}.pem",
            std::process::id()
        ));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == IoErrorKind::AlreadyExists => {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "the generated curl test CA path already exists",
                ));
            }
            Err(error) => {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("failed to create the curl test CA file: {error}"),
                ));
            }
        };
        if let Err(error) = file.write_all(pem) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(Error::new(
                ErrorKind::Internal,
                format!("failed to write the curl test CA file: {error}"),
            ));
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
impl Drop for TestCaFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct ActiveTransfer {
    handle: Easy2Handle<ResponseCollector>,
    request: Request,
    redirect_hops: u8,
    started: Instant,
}

struct ResponseCollector {
    headers: Vec<Header>,
    body: Vec<u8>,
    limits: ResponseLimits,
    header_bytes: usize,
    header_count: usize,
    failure: Option<CollectorFailure>,
    inactivity_timeout: Option<Duration>,
    last_activity: Instant,
    last_downloaded: f64,
    last_uploaded: f64,
}

#[derive(Clone, Copy)]
enum CollectorFailure {
    MalformedHeader,
    Limit(LimitKind),
    InactivityTimeout,
}

impl ResponseCollector {
    fn new(limits: ResponseLimits, inactivity_timeout: Option<Duration>) -> Self {
        Self {
            headers: Vec::new(),
            body: Vec::new(),
            limits,
            header_bytes: 0,
            header_count: 0,
            failure: None,
            inactivity_timeout,
            last_activity: Instant::now(),
            last_downloaded: 0.0,
            last_uploaded: 0.0,
        }
    }

    fn note_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    fn fail(&mut self, failure: CollectorFailure) {
        if self.failure.is_none() {
            self.failure = Some(failure);
        }
    }

    fn inactivity_expired(&self) -> bool {
        self.inactivity_timeout
            .is_some_and(|timeout| self.last_activity.elapsed() >= timeout)
    }
}

impl Handler for ResponseCollector {
    fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        self.note_activity();
        if self
            .body
            .len()
            .checked_add(data.len())
            .is_none_or(|size| size > self.limits.body_bytes)
        {
            self.fail(CollectorFailure::Limit(LimitKind::ResponseBodyBytes));
            return Ok(0);
        }
        self.body.extend_from_slice(data);
        Ok(data.len())
    }

    fn header(&mut self, data: &[u8]) -> bool {
        self.note_activity();
        let line = trim_line_end(data);
        if line.starts_with(b"HTTP/") {
            self.headers.clear();
            self.header_bytes = data.len();
            self.header_count = 0;
            if self.header_bytes > self.limits.header_bytes {
                self.fail(CollectorFailure::Limit(LimitKind::ResponseHeaderBytes));
                return false;
            }
            return true;
        }
        if self
            .header_bytes
            .checked_add(data.len())
            .is_none_or(|size| size > self.limits.header_bytes)
        {
            self.fail(CollectorFailure::Limit(LimitKind::ResponseHeaderBytes));
            return false;
        }
        self.header_bytes += data.len();
        if line.is_empty() {
            return true;
        }
        if self.header_count >= self.limits.header_count {
            self.fail(CollectorFailure::Limit(LimitKind::ResponseHeaderCount));
            return false;
        }
        self.header_count += 1;
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            self.fail(CollectorFailure::MalformedHeader);
            return false;
        };
        let Ok(name) = std::str::from_utf8(&line[..colon]) else {
            self.fail(CollectorFailure::MalformedHeader);
            return false;
        };
        if name.is_empty() {
            self.fail(CollectorFailure::MalformedHeader);
            return false;
        }
        let value = trim_header_value(&line[colon + 1..]);
        self.headers.push(Header::new(name, value.to_vec()));
        true
    }

    fn progress(
        &mut self,
        _download_total: f64,
        downloaded: f64,
        _upload_total: f64,
        uploaded: f64,
    ) -> bool {
        if downloaded != self.last_downloaded || uploaded != self.last_uploaded {
            self.last_downloaded = downloaded;
            self.last_uploaded = uploaded;
            self.note_activity();
        }
        if self
            .inactivity_timeout
            .is_some_and(|timeout| self.last_activity.elapsed() >= timeout)
        {
            self.fail(CollectorFailure::InactivityTimeout);
            return false;
        }
        true
    }
}

impl Backend for CurlBackend {
    fn submit(
        &mut self,
        id: RequestId,
        request: Request,
        accepted_at: Instant,
    ) -> Option<Completion> {
        self.start_transfer(id, request, 0, accepted_at)
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
        completions.extend(self.collect_inactivity_timeouts()?);
        if completions.is_empty() && !self.handles.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                self.multi.poll(&mut [], remaining).map_err(multi_error)?;
                self.multi.perform().map_err(multi_error)?;
                completions.extend(self.collect_completions()?);
                completions.extend(self.collect_inactivity_timeouts()?);
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
            if let Err(error) = self.multi.remove2(active.handle) {
                if failure.is_none() {
                    failure = Some(multi_error(error));
                }
            }
        }
        if let Some(error) = failure {
            return Err(ShutdownError::new(error));
        }
        Ok(())
    }

    fn poll_mode(&self) -> PollMode {
        PollMode::Interruptible {
            max_wait: Duration::from_millis(50),
        }
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
        let easy = configured_easy(
            &request,
            started,
            self.limits,
            #[cfg(test)]
            self.test_ca.as_ref(),
        )?;
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
            let ActiveTransfer {
                handle,
                request,
                redirect_hops,
                started,
            } = active;
            let easy = self.multi.remove2(handle).map_err(multi_error)?;
            let collector_failure = easy.get_ref().failure;
            let completion = match collector_failure {
                Some(failure) => Err(collector_error(failure, easy.get_ref().limits)),
                None => match result {
                    Ok(()) => match redirect_request(&easy, &request, redirect_hops) {
                        Ok(Some(request)) => {
                            match self.start_transfer(
                                id,
                                request,
                                redirect_hops.saturating_add(1),
                                started,
                            ) {
                                Ok(()) => continue,
                                Err(error) => Err(error),
                            }
                        }
                        Ok(None) => completed_response(&easy),
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(curl_transfer_error(
                        error,
                        &easy,
                        request.options(),
                        started,
                    )),
                },
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

    fn collect_inactivity_timeouts(&mut self) -> Result<Vec<BackendCompletion>, Error> {
        let expired = self
            .handles
            .iter()
            .filter(|(_id, active)| active.handle.get_ref().inactivity_expired())
            .map(|(id, _active)| *id)
            .collect::<Vec<_>>();
        let mut completions = Vec::with_capacity(expired.len());
        for id in expired {
            let active = self.handles.remove(&id).ok_or_else(|| {
                Error::new(
                    ErrorKind::Internal,
                    "curl inactivity timeout lost its owned easy handle",
                )
            })?;
            self.remove_token(id);
            self.multi.remove2(active.handle).map_err(multi_error)?;
            completions.push(BackendCompletion {
                id,
                completion: Completion::Failed(Error::timeout(
                    TimeoutKind::Inactivity,
                    "the request inactivity timeout expired",
                )),
            });
        }
        Ok(completions)
    }
}

fn configured_easy(
    request: &Request,
    started: Instant,
    limits: ResponseLimits,
    #[cfg(test)] test_ca: Option<&TestCa>,
) -> Result<Easy2<ResponseCollector>, Error> {
    validate_request(request)?;
    let mut easy = Easy2::new(ResponseCollector::new(
        limits,
        request.options().inactivity_timeout,
    ));
    easy.url(request.url()).map_err(curl_error)?;
    easy.proxy("").map_err(curl_error)?;
    easy.http_version(HttpVersion::V11).map_err(curl_error)?;
    easy.follow_location(false).map_err(curl_error)?;
    #[cfg(test)]
    if std::env::var_os("NBREQ_CURL_VERBOSE").is_some() {
        easy.verbose(true).map_err(curl_error)?;
    }

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
    let has_content_type = request
        .headers()
        .iter()
        .any(|header| header.name().eq_ignore_ascii_case("content-type"));
    for header in request.headers() {
        let value = std::str::from_utf8(header.value()).map_err(|_| {
            Error::new(
                ErrorKind::Unsupported,
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
    if !body.is_empty() && !has_content_type {
        headers.append("Content-Type:").map_err(curl_error)?;
    }
    easy.http_headers(headers).map_err(curl_error)?;

    let options = request.options();
    #[cfg(test)]
    if let Some(test_ca) = test_ca {
        match test_ca {
            TestCa::Blob(pem) => easy.ssl_cainfo_blob(pem).map_err(curl_error)?,
            TestCa::File(file) => easy.cainfo(file.path()).map_err(curl_error)?,
        }
    }
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
    if options.inactivity_timeout.is_some() {
        easy.progress(true).map_err(curl_error)?;
    }
    Ok(easy)
}

fn validate_request(request: &Request) -> Result<(), Error> {
    request.validate()
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
        return Err(Error::timeout(
            TimeoutKind::Total,
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

fn completed_response(easy: &Easy2<ResponseCollector>) -> Result<Response, Error> {
    let status = easy.response_code().map_err(curl_error)?;
    let status = u16::try_from(status).map_err(|_| {
        Error::transport(
            TransportStage::Http,
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
    if error.is_couldnt_resolve_host() || error.is_couldnt_resolve_proxy() {
        Error::transport(
            TransportStage::Dns,
            format!("curl name resolution failed: {}", error.description()),
        )
    } else if error.is_couldnt_connect() {
        Error::transport(
            TransportStage::Connect,
            format!("curl connection failed: {}", error.description()),
        )
    } else if is_tls_error(&error) {
        Error::transport(
            TransportStage::Tls,
            format!("curl TLS operation failed: {}", error.description()),
        )
    } else if error.is_read_error() || error.is_send_error() || error.is_upload_failed() {
        Error::transport(
            TransportStage::Send,
            format!("curl request send failed: {}", error.description()),
        )
    } else if error.is_recv_error()
        || error.is_partial_file()
        || error.is_got_nothing()
        || error.is_write_error()
    {
        Error::transport(
            TransportStage::Receive,
            format!("curl response receive failed: {}", error.description()),
        )
    } else {
        Error::new(
            ErrorKind::Transport,
            format!("curl transfer failed: {}", error.description()),
        )
    }
}

fn curl_transfer_error(
    error: curl::Error,
    easy: &Easy2<ResponseCollector>,
    options: &crate::RequestOptions,
    started: Instant,
) -> Error {
    if !error.is_operation_timedout() {
        return curl_error(error);
    }
    let total_expired = options
        .total_timeout
        .is_some_and(|timeout| started.elapsed() >= timeout);
    let connected = easy.primary_ip().is_ok_and(|address| address.is_some())
        || easy
            .connect_time()
            .is_ok_and(|duration| !duration.is_zero());
    if total_expired || (connected && options.total_timeout.is_some()) {
        Error::timeout(TimeoutKind::Total, "the total request timeout expired")
    } else if !connected {
        Error::timeout(
            TimeoutKind::Connect,
            "the connection-establishment timeout expired",
        )
    } else {
        Error::timeout(
            TimeoutKind::Unknown,
            "curl reported a timeout after connection establishment without an NBReq total deadline",
        )
    }
}

fn collector_error(failure: CollectorFailure, limits: ResponseLimits) -> Error {
    match failure {
        CollectorFailure::MalformedHeader => Error::transport(
            TransportStage::Http,
            "curl rejected a malformed response header",
        ),
        CollectorFailure::Limit(LimitKind::ResponseBodyBytes) => Error::limit(
            LimitKind::ResponseBodyBytes,
            format!(
                "response body exceeds the configured {} byte limit",
                limits.body_bytes
            ),
        ),
        CollectorFailure::Limit(LimitKind::ResponseHeaderBytes) => Error::limit(
            LimitKind::ResponseHeaderBytes,
            format!(
                "response headers exceed the configured {} byte limit",
                limits.header_bytes
            ),
        ),
        CollectorFailure::Limit(LimitKind::ResponseHeaderCount) => Error::limit(
            LimitKind::ResponseHeaderCount,
            format!(
                "response headers exceed the configured {} field limit",
                limits.header_count
            ),
        ),
        CollectorFailure::Limit(other) => Error::limit(other, "a response limit was exceeded"),
        CollectorFailure::InactivityTimeout => Error::timeout(
            TimeoutKind::Inactivity,
            "the request inactivity timeout expired",
        ),
    }
}

fn is_tls_error(error: &curl::Error) -> bool {
    error.is_ssl_connect_error()
        || error.is_peer_failed_verification()
        || error.is_ssl_certproblem()
        || error.is_ssl_cipher()
        || error.is_ssl_cacert()
        || error.is_ssl_cacert_badfile()
        || error.is_ssl_crl_badfile()
        || error.is_ssl_issuer_error()
}

fn multi_error(error: curl::MultiError) -> Error {
    Error::new(
        ErrorKind::Internal,
        format!("curl Multi operation failed: {}", error.description()),
    )
}
