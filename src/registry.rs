use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use crate::atomic::try_update_usize;
use crate::context;
use crate::dispatch::{CallbackDomain, CallbackJob};
use crate::metrics::{EngineMetrics, Metrics};
use crate::stream::{ResponseControl, ResponseSink, response_pair};
use crate::tcp::io::{TcpAbort, TcpIoConfig, TcpIoOwner, TcpIoShared};
use crate::{
    Client, Completion, EngineConfig, Error, ErrorKind, LimitKind, Request, RequestHandle,
    RequestId, ResponseReader, RunMode, StreamRequest, TcpConnectCompletion, TcpConnectRequest,
    TcpConnection, TcpConnectionHandle, TcpConnector,
};
#[cfg(feature = "resolver")]
use crate::{ResolveCompletion, ResolveRequest};

pub(crate) type CompletionCallback = Box<dyn FnOnce(Completion) + Send + 'static>;
#[cfg(feature = "resolver")]
pub(crate) type ResolveCallback = Box<dyn FnOnce(ResolveCompletion) + Send + 'static>;
pub(crate) type TcpConnectCallback = Box<dyn FnOnce(TcpConnectCompletion) + Send + 'static>;
type ExternalWaker = Arc<dyn Fn() -> Result<(), Error> + Send + Sync + 'static>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleState {
    Running,
    ShuttingDown,
    Stopped,
}

pub(crate) struct AdmissionPermit {
    inflight: Arc<AtomicUsize>,
}

pub(crate) struct BytePermit {
    used: Arc<AtomicUsize>,
    bytes: usize,
}

impl BytePermit {
    fn try_acquire(used: &Arc<AtomicUsize>, limit: usize, bytes: usize) -> Option<Self> {
        try_update_usize(used, Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(bytes).filter(|next| *next <= limit)
        })
        .ok()?;
        Some(Self {
            used: Arc::clone(used),
            bytes,
        })
    }
}

impl Drop for BytePermit {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl AdmissionPermit {
    fn try_acquire(inflight: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        try_update_usize(inflight, Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < limit).then_some(current + 1)
        })
        .ok()?;
        Some(Self {
            inflight: Arc::clone(inflight),
        })
    }
}

struct RequestInner {
    completion: Option<Completion>,
    callback: Option<CompletionCallback>,
    callback_active: bool,
    inflight_permit: Option<AdmissionPermit>,
    callback_permit: Option<AdmissionPermit>,
}

pub(crate) struct RequestState {
    id: RequestId,
    inner: Mutex<RequestInner>,
    changed: Condvar,
    metrics: Arc<Metrics>,
}

impl fmt::Debug for RequestState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestState")
            .field("id", &self.id)
            .field("terminal", &self.is_terminal())
            .finish()
    }
}

impl RequestState {
    fn new(
        id: RequestId,
        callback: Option<CompletionCallback>,
        inflight_permit: AdmissionPermit,
        callback_permit: Option<AdmissionPermit>,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            inner: Mutex::new(RequestInner {
                completion: None,
                callback,
                callback_active: false,
                inflight_permit: Some(inflight_permit),
                callback_permit,
            }),
            changed: Condvar::new(),
            metrics,
        })
    }

    pub(crate) fn id(&self) -> RequestId {
        self.id
    }

    pub(crate) fn is_terminal(&self) -> bool {
        lock_unpoisoned(&self.inner).completion.is_some()
    }

    pub(crate) fn completion(&self) -> Option<Completion> {
        lock_unpoisoned(&self.inner).completion.clone()
    }

    pub(crate) fn wait(&self) -> Completion {
        assert!(
            !context::is_active(self.id.engine),
            "blocking wait on the active drive/callback stack is forbidden"
        );
        let inner = lock_unpoisoned(&self.inner);
        let inner = self
            .changed
            .wait_while(inner, |inner| inner.completion.is_none())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner
            .completion
            .clone()
            .expect("terminal wait predicate completed without a result")
    }

    pub(crate) fn wait_for(&self, duration: std::time::Duration) -> Option<Completion> {
        assert!(
            !context::is_active(self.id.engine),
            "blocking wait on the active drive/callback stack is forbidden"
        );
        let inner = lock_unpoisoned(&self.inner);
        let (inner, _timeout) = self
            .changed
            .wait_timeout_while(inner, duration, |inner| inner.completion.is_none())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.completion.clone()
    }

    fn commit(&self, completion: Completion) -> (bool, Option<CallbackJob>) {
        let mut inner = lock_unpoisoned(&self.inner);
        if inner.completion.is_some() {
            return (false, None);
        }

        self.metrics.request_terminal(&completion);
        inner.completion = Some(completion);
        self.changed.notify_all();
        let job = Self::take_terminal_job(self.id, &mut inner);
        if inner.callback.is_none() && job.is_none() {
            drop(inner.inflight_permit.take());
            drop(inner.callback_permit.take());
        }
        (true, job)
    }

    fn activate_callback(&self) -> Option<CallbackJob> {
        let mut inner = lock_unpoisoned(&self.inner);
        inner.callback_active = true;
        Self::take_terminal_job(self.id, &mut inner)
    }

    fn take_terminal_job(id: RequestId, inner: &mut RequestInner) -> Option<CallbackJob> {
        if !inner.callback_active || inner.completion.is_none() {
            return None;
        }
        let callback = inner.callback.take()?;
        let completion = inner
            .completion
            .clone()
            .expect("terminal callback requires canonical completion");
        let inflight_permit = inner
            .inflight_permit
            .take()
            .expect("terminal callback requires its admission permit");
        let callback_permit = inner
            .callback_permit
            .take()
            .expect("terminal callback requires its callback-capacity permit");
        Some(CallbackJob::new(id, move || {
            let _inflight_permit = inflight_permit;
            let _callback_permit = callback_permit;
            callback(completion);
        }))
    }
}

#[cfg(feature = "resolver")]
struct ResolveInner {
    completion: Option<ResolveCompletion>,
    callback: Option<ResolveCallback>,
    callback_active: bool,
    inflight_permit: Option<AdmissionPermit>,
    callback_permit: Option<AdmissionPermit>,
}

#[cfg(feature = "resolver")]
pub(crate) struct ResolveState {
    id: RequestId,
    inner: Mutex<ResolveInner>,
    changed: Condvar,
    metrics: Arc<Metrics>,
}

#[cfg(feature = "resolver")]
impl fmt::Debug for ResolveState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolveState")
            .field("id", &self.id)
            .field("terminal", &self.is_terminal())
            .finish()
    }
}

#[cfg(feature = "resolver")]
impl ResolveState {
    fn new(
        id: RequestId,
        callback: Option<ResolveCallback>,
        inflight_permit: AdmissionPermit,
        callback_permit: Option<AdmissionPermit>,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            inner: Mutex::new(ResolveInner {
                completion: None,
                callback,
                callback_active: false,
                inflight_permit: Some(inflight_permit),
                callback_permit,
            }),
            changed: Condvar::new(),
            metrics,
        })
    }

    pub(crate) fn id(&self) -> RequestId {
        self.id
    }

    pub(crate) fn is_terminal(&self) -> bool {
        lock_unpoisoned(&self.inner).completion.is_some()
    }

    pub(crate) fn completion(&self) -> Option<ResolveCompletion> {
        lock_unpoisoned(&self.inner).completion.clone()
    }

    pub(crate) fn wait(&self) -> ResolveCompletion {
        assert!(
            !context::is_active(self.id.engine),
            "blocking wait on the active drive/callback stack is forbidden"
        );
        let inner = lock_unpoisoned(&self.inner);
        let inner = self
            .changed
            .wait_while(inner, |inner| inner.completion.is_none())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner
            .completion
            .clone()
            .expect("terminal wait predicate completed without a result")
    }

    pub(crate) fn wait_for(&self, duration: std::time::Duration) -> Option<ResolveCompletion> {
        assert!(
            !context::is_active(self.id.engine),
            "blocking wait on the active drive/callback stack is forbidden"
        );
        let inner = lock_unpoisoned(&self.inner);
        let (inner, _timeout) = self
            .changed
            .wait_timeout_while(inner, duration, |inner| inner.completion.is_none())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.completion.clone()
    }

    fn commit(&self, completion: ResolveCompletion) -> (bool, Option<CallbackJob>) {
        let mut inner = lock_unpoisoned(&self.inner);
        if inner.completion.is_some() {
            return (false, None);
        }

        self.metrics.resolution_terminal(&completion);
        inner.completion = Some(completion);
        self.changed.notify_all();
        let job = Self::take_terminal_job(self.id, &mut inner);
        if inner.callback.is_none() && job.is_none() {
            drop(inner.inflight_permit.take());
            drop(inner.callback_permit.take());
        }
        (true, job)
    }

    fn activate_callback(&self) -> Option<CallbackJob> {
        let mut inner = lock_unpoisoned(&self.inner);
        inner.callback_active = true;
        Self::take_terminal_job(self.id, &mut inner)
    }

    fn take_terminal_job(id: RequestId, inner: &mut ResolveInner) -> Option<CallbackJob> {
        if !inner.callback_active || inner.completion.is_none() {
            return None;
        }
        let callback = inner.callback.take()?;
        let completion = inner
            .completion
            .clone()
            .expect("terminal callback requires canonical completion");
        let inflight_permit = inner
            .inflight_permit
            .take()
            .expect("terminal callback requires its admission permit");
        let callback_permit = inner
            .callback_permit
            .take()
            .expect("terminal callback requires its callback-capacity permit");
        Some(CallbackJob::new(id, move || {
            let _inflight_permit = inflight_permit;
            let _callback_permit = callback_permit;
            callback(completion);
        }))
    }
}

struct TcpConnectInner {
    completion: Option<TcpConnectCompletion>,
    terminal: bool,
    delivery_ready: bool,
    callback: Option<TcpConnectCallback>,
    callback_active: bool,
    occupancy_permit: Option<AdmissionPermit>,
    queued_bytes_permit: Option<BytePermit>,
    metric_bytes_permit: Option<BytePermit>,
    dns_borrow: Option<AdmissionPermit>,
    callback_permit: Option<AdmissionPermit>,
}

pub(crate) struct TcpConnectState {
    id: RequestId,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    connector: TcpConnector,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    send_window: usize,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    receive_window: usize,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    read_inactivity_timeout: Option<Duration>,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    write_inactivity_timeout: Option<Duration>,
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    max_resolve_results: usize,
    inner: Mutex<TcpConnectInner>,
    changed: Condvar,
    metrics: Arc<Metrics>,
}

struct TcpConnectAdmission {
    connector: TcpConnector,
    send_window: usize,
    receive_window: usize,
    read_inactivity_timeout: Option<Duration>,
    write_inactivity_timeout: Option<Duration>,
    max_resolve_results: usize,
    callback: Option<TcpConnectCallback>,
    occupancy_permit: AdmissionPermit,
    queued_bytes_permit: BytePermit,
    metric_bytes_permit: BytePermit,
    dns_borrow: Option<AdmissionPermit>,
    callback_permit: Option<AdmissionPermit>,
    metrics: Arc<Metrics>,
}

impl fmt::Debug for TcpConnectState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TcpConnectState")
            .field("id", &self.id)
            .field("terminal", &self.is_terminal())
            .finish()
    }
}

impl TcpConnectState {
    fn new(id: RequestId, admission: TcpConnectAdmission) -> Arc<Self> {
        Arc::new(Self {
            id,
            connector: admission.connector,
            send_window: admission.send_window,
            receive_window: admission.receive_window,
            read_inactivity_timeout: admission.read_inactivity_timeout,
            write_inactivity_timeout: admission.write_inactivity_timeout,
            max_resolve_results: admission.max_resolve_results,
            inner: Mutex::new(TcpConnectInner {
                completion: None,
                terminal: false,
                delivery_ready: false,
                callback: admission.callback,
                callback_active: false,
                occupancy_permit: Some(admission.occupancy_permit),
                queued_bytes_permit: Some(admission.queued_bytes_permit),
                metric_bytes_permit: Some(admission.metric_bytes_permit),
                dns_borrow: admission.dns_borrow,
                callback_permit: admission.callback_permit,
            }),
            changed: Condvar::new(),
            metrics: admission.metrics,
        })
    }

    pub(crate) fn id(&self) -> RequestId {
        self.id
    }

    pub(crate) fn is_terminal(&self) -> bool {
        lock_unpoisoned(&self.inner).terminal
    }

    pub(crate) fn try_completion(&self) -> Option<TcpConnectCompletion> {
        let mut inner = lock_unpoisoned(&self.inner);
        inner
            .delivery_ready
            .then(|| inner.completion.take())
            .flatten()
    }

    pub(crate) fn wait(&self) -> TcpConnectCompletion {
        assert!(
            !context::is_active(self.id.engine),
            "blocking wait on the active drive/callback stack is forbidden"
        );
        let inner = lock_unpoisoned(&self.inner);
        let mut inner = self
            .changed
            .wait_while(inner, |inner| !inner.delivery_ready)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner
            .completion
            .take()
            .expect("terminal TCP connect waiter has already been consumed")
    }

    pub(crate) fn wait_for(&self, duration: std::time::Duration) -> Option<TcpConnectCompletion> {
        assert!(
            !context::is_active(self.id.engine),
            "blocking wait on the active drive/callback stack is forbidden"
        );
        let inner = lock_unpoisoned(&self.inner);
        let (mut inner, _timeout) = self
            .changed
            .wait_timeout_while(inner, duration, |inner| !inner.delivery_ready)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner.delivery_ready {
            Some(
                inner
                    .completion
                    .take()
                    .expect("terminal TCP connect waiter has already been consumed"),
            )
        } else {
            None
        }
    }

    fn commit_terminal(&self, completion: TcpConnectCompletion) -> bool {
        let mut inner = lock_unpoisoned(&self.inner);
        if inner.terminal {
            return false;
        }
        self.metrics.tcp_connect_terminal(&completion);
        inner.terminal = true;
        inner.completion = Some(completion);
        drop(inner.occupancy_permit.take());
        drop(inner.queued_bytes_permit.take());
        drop(inner.metric_bytes_permit.take());
        drop(inner.dns_borrow.take());
        true
    }

    fn publish_terminal(&self) -> Option<CallbackJob> {
        let mut inner = lock_unpoisoned(&self.inner);
        debug_assert!(
            inner.terminal,
            "only a terminal TCP connect can be published"
        );
        debug_assert!(
            !inner.delivery_ready,
            "a TCP connect terminal must be published exactly once"
        );
        inner.delivery_ready = true;
        let job = Self::take_terminal_job(self.id, &mut inner);
        self.changed.notify_all();
        job
    }

    fn activate_callback(&self) -> Option<CallbackJob> {
        let mut inner = lock_unpoisoned(&self.inner);
        inner.callback_active = true;
        Self::take_terminal_job(self.id, &mut inner)
    }

    fn take_terminal_job(id: RequestId, inner: &mut TcpConnectInner) -> Option<CallbackJob> {
        if !inner.callback_active || !inner.delivery_ready {
            return None;
        }
        let callback = inner.callback.take()?;
        let completion = inner
            .completion
            .take()
            .expect("terminal TCP connect callback requires its completion");
        let callback_permit = inner
            .callback_permit
            .take()
            .expect("terminal TCP connect callback requires its callback permit");
        Some(CallbackJob::new(id, move || {
            let _callback_permit = callback_permit;
            callback(completion);
        }))
    }
}

pub(crate) struct StreamRequestState {
    id: RequestId,
    control: ResponseControl,
    permits: Mutex<Option<(AdmissionPermit, BytePermit, BytePermit)>>,
}

impl StreamRequestState {
    fn new(
        id: RequestId,
        control: ResponseControl,
        inflight_permit: AdmissionPermit,
        queued_bytes_permit: BytePermit,
        parent_queued_bytes_permit: BytePermit,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            control,
            permits: Mutex::new(Some((
                inflight_permit,
                queued_bytes_permit,
                parent_queued_bytes_permit,
            ))),
        })
    }

    pub(crate) fn id(&self) -> RequestId {
        self.id
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.control.is_terminal()
    }

    fn cancel(&self) -> bool {
        let won = self.control.cancel();
        if won {
            self.release_permits();
        }
        won
    }

    fn fail(&self, error: Error) -> bool {
        let won = self.control.fail(error);
        if won {
            self.release_permits();
        }
        won
    }

    fn release_permits(&self) {
        drop(lock_unpoisoned(&self.permits).take());
    }
}

pub(crate) enum Submission {
    Buffered {
        request: Request,
        state: Arc<RequestState>,
        accepted_at: Instant,
    },
    Stream {
        request: StreamRequest,
        state: Arc<StreamRequestState>,
        response: ResponseSink,
        accepted_at: Instant,
    },
    #[cfg(feature = "resolver")]
    Resolve {
        request: ResolveRequest,
        state: Arc<ResolveState>,
        accepted_at: Instant,
        max_results: usize,
    },
    Connect {
        request: TcpConnectRequest,
        state: Arc<TcpConnectState>,
        accepted_at: Instant,
    },
}

struct QueueState {
    submissions: VecDeque<Submission>,
    generation: u64,
}

pub(crate) struct CommandQueue {
    capacity: usize,
    state: Mutex<QueueState>,
    changed: Condvar,
    external_waker: Mutex<Option<ExternalWaker>>,
    external_wake_failure: Mutex<Option<Error>>,
    metrics: Arc<Metrics>,
}

impl CommandQueue {
    fn new(capacity: usize, metrics: Arc<Metrics>) -> Self {
        Self {
            capacity,
            state: Mutex::new(QueueState {
                submissions: VecDeque::new(),
                generation: 0,
            }),
            changed: Condvar::new(),
            external_waker: Mutex::new(None),
            external_wake_failure: Mutex::new(None),
            metrics,
        }
    }

    fn try_push(&self, submission: Submission) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        if state.submissions.len() >= self.capacity {
            return false;
        }
        state.submissions.push_back(submission);
        self.metrics.command_queued();
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_one();
        drop(state);
        self.wake_external();
        true
    }

    pub(crate) fn drain(&self) -> Vec<Submission> {
        let drained = lock_unpoisoned(&self.state)
            .submissions
            .drain(..)
            .collect::<Vec<_>>();
        self.metrics.commands_drained(drained.len());
        drained
    }

    pub(crate) fn wake(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
        drop(state);
        self.wake_external();
    }

    pub(crate) fn wait_for_signal(&self, seen_generation: &mut u64, stopping: &AtomicBool) {
        let mut state = lock_unpoisoned(&self.state);
        while state.submissions.is_empty()
            && state.generation == *seen_generation
            && !stopping.load(Ordering::Acquire)
        {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *seen_generation = state.generation;
    }

    pub(crate) fn generation(&self) -> u64 {
        lock_unpoisoned(&self.state).generation
    }

    pub(crate) fn wait_for_signal_until(
        &self,
        seen_generation: u64,
        stopping: &AtomicBool,
        deadline: Instant,
    ) -> bool {
        let duration = deadline.saturating_duration_since(Instant::now());
        if duration.is_zero() {
            return false;
        }
        let state = lock_unpoisoned(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, duration, |state| {
                state.submissions.is_empty()
                    && state.generation == seen_generation
                    && !stopping.load(Ordering::Acquire)
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !timeout.timed_out()
            || !state.submissions.is_empty()
            || state.generation != seen_generation
            || stopping.load(Ordering::Acquire)
    }

    pub(crate) fn set_external_waker(&self, waker: Option<ExternalWaker>) {
        *lock_unpoisoned(&self.external_waker) = waker;
    }

    pub(crate) fn take_external_wake_failure(&self) -> Option<Error> {
        lock_unpoisoned(&self.external_wake_failure).take()
    }

    fn wake_external(&self) {
        let waker = lock_unpoisoned(&self.external_waker).clone();
        if let Some(waker) = waker {
            if let Err(error) = waker() {
                let mut failure = lock_unpoisoned(&self.external_wake_failure);
                if failure.is_none() {
                    *failure = Some(error);
                }
            }
        }
    }
}

struct CoreState {
    lifecycle: LifecycleState,
    next_sequence: u64,
    requests: HashMap<RequestId, Arc<RequestState>>,
    stream_requests: HashMap<RequestId, Arc<StreamRequestState>>,
    #[cfg(feature = "resolver")]
    resolutions: HashMap<RequestId, Arc<ResolveState>>,
    connects: HashMap<RequestId, Arc<TcpConnectState>>,
    live_tcp: HashMap<RequestId, Arc<TcpIoShared>>,
}

pub(crate) struct AcceptedRequest {
    pub(crate) state: Arc<RequestState>,
}

#[cfg(feature = "resolver")]
pub(crate) struct AcceptedResolve {
    pub(crate) state: Arc<ResolveState>,
}

pub(crate) struct AcceptedTcpConnect {
    pub(crate) state: Arc<TcpConnectState>,
}

pub(crate) struct TcpConnectSink {
    shared: Weak<Shared>,
    state: Arc<TcpConnectState>,
}

#[cfg_attr(not(feature = "native"), allow(dead_code))]
impl TcpConnectSink {
    pub(crate) fn new(shared: &Arc<Shared>, state: Arc<TcpConnectState>) -> Self {
        Self {
            shared: Arc::downgrade(shared),
            state,
        }
    }

    pub(crate) fn id(&self) -> RequestId {
        self.state.id()
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    pub(crate) fn send_window(&self) -> usize {
        self.state.send_window
    }

    pub(crate) fn receive_window(&self) -> usize {
        self.state.receive_window
    }

    pub(crate) fn read_inactivity_timeout(&self) -> Option<Duration> {
        self.state.read_inactivity_timeout
    }

    pub(crate) fn write_inactivity_timeout(&self) -> Option<Duration> {
        self.state.write_inactivity_timeout
    }

    pub(crate) fn max_resolve_results(&self) -> usize {
        self.state.max_resolve_results
    }

    pub(crate) fn release_dns_borrow(&self) -> bool {
        let mut inner = lock_unpoisoned(&self.state.inner);
        if inner.terminal {
            return false;
        }
        let Some(permit) = inner.dns_borrow.take() else {
            return false;
        };
        drop(permit);
        true
    }

    pub(crate) fn connected(
        &self,
        local: std::net::SocketAddr,
        peer: std::net::SocketAddr,
    ) -> Option<TcpIoOwner> {
        self.shared
            .upgrade()?
            .complete_tcp_connected(&self.state, local, peer)
    }

    pub(crate) fn fail(&self, error: Error) -> bool {
        self.shared.upgrade().is_some_and(|shared| {
            shared.complete_tcp_state(&self.state, TcpConnectCompletion::Failed(error))
        })
    }
}

pub(crate) struct Shared {
    pub(crate) id: u64,
    pub(crate) stopped: AtomicBool,
    pub(crate) run_mode: RunMode,
    pub(crate) queue: CommandQueue,
    pub(crate) metrics: Arc<Metrics>,
    callback_domain: Arc<CallbackDomain>,
    core: Mutex<CoreState>,
    inflight: Arc<AtomicUsize>,
    inflight_limit: usize,
    resolution_inflight: Arc<AtomicUsize>,
    resolution_inflight_limit: usize,
    native_resolver_supported: AtomicBool,
    standalone_tcp_supported: AtomicBool,
    callback_inflight: Arc<AtomicUsize>,
    callback_inflight_limit: usize,
    streaming_supported: bool,
    max_request_body_bytes: usize,
    max_response_body_bytes: usize,
    max_stream_queue_bytes_per_request: usize,
    stream_queued_bytes: Arc<AtomicUsize>,
    max_stream_queued_bytes: usize,
    queued_bytes: Arc<AtomicUsize>,
    max_queued_bytes: usize,
    tcp_connections: Arc<AtomicUsize>,
    tcp_queued_bytes: Arc<AtomicUsize>,
    tcp_connection_limit: usize,
    max_tcp_queue_bytes_per_connection: usize,
    max_resolve_results: usize,
    max_header_bytes: usize,
    max_header_count: usize,
    callback_activations: Mutex<usize>,
    callback_activations_done: Condvar,
    #[cfg(test)]
    callback_activation_hook: Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
    #[cfg(test)]
    tcp_terminal_commit_hook: Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
}

pub(crate) struct CallbackActivation<'shared> {
    shared: &'shared Shared,
}

impl Drop for CallbackActivation<'_> {
    fn drop(&mut self) {
        self.shared.finish_callback_activation();
    }
}

impl fmt::Debug for Shared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Shared")
            .field("id", &self.id)
            .field("stopped", &self.stopped.load(Ordering::Acquire))
            .field("run_mode", &self.run_mode)
            .finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn new(
        id: u64,
        config: &EngineConfig,
        callback_domain: Arc<CallbackDomain>,
        streaming_supported: bool,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        let command_capacity = config.command_queue_capacity().get();
        let callback_capacity = config.callback_queue_capacity().get();
        Arc::new(Self {
            id,
            stopped: AtomicBool::new(false),
            run_mode: config.run_mode(),
            queue: CommandQueue::new(command_capacity, Arc::clone(&metrics)),
            metrics,
            callback_domain,
            core: Mutex::new(CoreState {
                lifecycle: LifecycleState::Running,
                next_sequence: 1,
                requests: HashMap::new(),
                stream_requests: HashMap::new(),
                #[cfg(feature = "resolver")]
                resolutions: HashMap::new(),
                connects: HashMap::new(),
                live_tcp: HashMap::new(),
            }),
            inflight: Arc::new(AtomicUsize::new(0)),
            inflight_limit: config.max_inflight_requests().get(),
            resolution_inflight: Arc::new(AtomicUsize::new(0)),
            resolution_inflight_limit: config.max_inflight_resolutions().get(),
            native_resolver_supported: AtomicBool::new(false),
            standalone_tcp_supported: AtomicBool::new(false),
            callback_inflight: Arc::new(AtomicUsize::new(0)),
            callback_inflight_limit: callback_capacity,
            streaming_supported,
            max_request_body_bytes: config.max_request_body_bytes(),
            max_response_body_bytes: config.max_response_body_bytes(),
            max_stream_queue_bytes_per_request: config.max_stream_queue_bytes_per_request(),
            stream_queued_bytes: Arc::new(AtomicUsize::new(0)),
            max_stream_queued_bytes: config.max_stream_queued_bytes(),
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            max_queued_bytes: config.max_queued_bytes(),
            tcp_connections: Arc::new(AtomicUsize::new(0)),
            tcp_queued_bytes: Arc::new(AtomicUsize::new(0)),
            tcp_connection_limit: config.max_standalone_tcp_connections().get(),
            max_tcp_queue_bytes_per_connection: config.max_tcp_queue_bytes_per_connection(),
            max_resolve_results: config.max_resolve_results().get(),
            max_header_bytes: config.max_header_bytes(),
            max_header_count: config.max_header_count(),
            callback_activations: Mutex::new(0),
            callback_activations_done: Condvar::new(),
            #[cfg(test)]
            callback_activation_hook: Mutex::new(None),
            #[cfg(test)]
            tcp_terminal_commit_hook: Mutex::new(None),
        })
    }

    pub(crate) fn accept(
        self: &Arc<Self>,
        request: Request,
        callback: Option<CompletionCallback>,
    ) -> Result<AcceptedRequest, Error> {
        let has_callback = callback.is_some();
        let mut core = lock_unpoisoned(&self.core);
        if core.lifecycle != LifecycleState::Running {
            return Err(Error::new(
                ErrorKind::EngineStopped,
                "the owning Engine has stopped accepting work",
            ));
        }
        if core.next_sequence == u64::MAX {
            return Err(Error::new(
                ErrorKind::Internal,
                "request identity space is exhausted",
            ));
        }
        self.validate_request_limits(&request)?;

        let inflight_permit = AdmissionPermit::try_acquire(&self.inflight, self.inflight_limit)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::QueueFull,
                    "the Engine's accepted/inflight request capacity is full",
                )
            })?;
        let callback_permit = if has_callback {
            Some(
                AdmissionPermit::try_acquire(&self.callback_inflight, self.callback_inflight_limit)
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::QueueFull,
                            "the Engine's callback request/event capacity is full",
                        )
                    })?,
            )
        } else {
            None
        };
        let id = RequestId {
            engine: self.id,
            sequence: core.next_sequence,
        };
        core.next_sequence += 1;
        let state = RequestState::new(
            id,
            callback,
            inflight_permit,
            callback_permit,
            Arc::clone(&self.metrics),
        );
        core.requests.insert(id, Arc::clone(&state));

        let submission = Submission::Buffered {
            request,
            state: Arc::clone(&state),
            accepted_at: Instant::now(),
        };
        if !self.queue.try_push(submission) {
            core.requests.remove(&id);
            return Err(Error::new(
                ErrorKind::QueueFull,
                "the Engine's bounded command queue is full",
            ));
        }
        self.metrics.request_accepted(
            self.inflight.load(Ordering::Acquire),
            self.stream_queued_bytes.load(Ordering::Acquire),
        );

        let activation = if has_callback {
            *lock_unpoisoned(&self.callback_activations) += 1;
            Some(CallbackActivation { shared: self })
        } else {
            None
        };
        drop(core);

        if has_callback {
            #[cfg(test)]
            self.run_callback_activation_hook();
            if let Some(job) = state.activate_callback() {
                let _queued = self.callback_domain.enqueue_terminal(job);
            }
        }
        drop(activation);
        Ok(AcceptedRequest { state })
    }

    pub(crate) fn set_native_resolver_supported(&self, supported: bool) {
        self.native_resolver_supported
            .store(supported, Ordering::Release);
    }

    pub(crate) fn native_resolver_supported(&self) -> bool {
        self.native_resolver_supported.load(Ordering::Acquire)
    }

    pub(crate) fn set_standalone_tcp_supported(&self, supported: bool) {
        self.standalone_tcp_supported
            .store(supported, Ordering::Release);
    }

    pub(crate) fn standalone_tcp_supported(&self) -> bool {
        self.standalone_tcp_supported.load(Ordering::Acquire)
    }

    pub(crate) fn accept_tcp_connect(
        self: &Arc<Self>,
        connector: TcpConnector,
        request: TcpConnectRequest,
        callback: Option<TcpConnectCallback>,
    ) -> Result<AcceptedTcpConnect, Error> {
        let has_callback = callback.is_some();
        let mut core = lock_unpoisoned(&self.core);
        if core.lifecycle != LifecycleState::Running {
            return Err(Error::new(
                ErrorKind::EngineStopped,
                "the owning Engine has stopped accepting work",
            ));
        }
        if !self.standalone_tcp_supported() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "standalone TCP connections are not available on this Engine",
            ));
        }
        let hostname = matches!(request.target(), crate::TcpConnectTarget::Hostname { .. });
        if hostname && !self.native_resolver_supported() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "hostname TCP connections require the native resolver owner",
            ));
        }
        let send_window = request
            .send_queue_bytes()
            .unwrap_or(self.max_tcp_queue_bytes_per_connection);
        let receive_window = request
            .receive_queue_bytes()
            .unwrap_or(self.max_tcp_queue_bytes_per_connection);
        let read_inactivity_timeout = request.read_inactivity_timeout();
        let write_inactivity_timeout = request.write_inactivity_timeout();
        if send_window > self.max_tcp_queue_bytes_per_connection
            || receive_window > self.max_tcp_queue_bytes_per_connection
        {
            return Err(Error::limit(
                LimitKind::TcpQueueBytes,
                format!(
                    "TCP queue window exceeds the Engine ceiling of {} bytes",
                    self.max_tcp_queue_bytes_per_connection
                ),
            ));
        }
        if send_window == 0 || receive_window == 0 {
            return Err(Error::limit(
                LimitKind::TcpQueueBytes,
                "standalone TCP is disabled by the Engine's zero-byte per-connection queue window",
            ));
        }
        let reserved_bytes = send_window.checked_add(receive_window).ok_or_else(|| {
            Error::limit(LimitKind::TcpQueueBytes, "TCP queue reservation overflowed")
        })?;
        if self.max_queued_bytes == 0 {
            return Err(Error::limit(
                LimitKind::TcpQueueBytes,
                "standalone TCP is disabled by the Engine's zero-byte shared queue budget",
            ));
        }
        if core.next_sequence == u64::MAX {
            return Err(Error::new(
                ErrorKind::Internal,
                "request identity space is exhausted",
            ));
        }
        let occupancy_permit =
            AdmissionPermit::try_acquire(&self.tcp_connections, self.tcp_connection_limit)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::QueueFull,
                        "the Engine's standalone TCP connection capacity is full",
                    )
                })?;
        let queued_bytes_permit =
            BytePermit::try_acquire(&self.queued_bytes, self.max_queued_bytes, reserved_bytes)
                .ok_or_else(|| {
                    Error::limit(
                        LimitKind::TcpQueueBytes,
                        format!(
                            "TCP queues exceed the Engine's configured {} byte shared queue budget",
                            self.max_queued_bytes
                        ),
                    )
                })?;
        let metric_bytes_permit =
            BytePermit::try_acquire(&self.tcp_queued_bytes, usize::MAX, reserved_bytes)
                .expect("accepted TCP reservation already fits usize");
        let dns_borrow = if hostname {
            Some(
                AdmissionPermit::try_acquire(
                    &self.resolution_inflight,
                    self.resolution_inflight_limit,
                )
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::QueueFull,
                        "the Engine's accepted/inflight public-resolution capacity is full",
                    )
                })?,
            )
        } else {
            None
        };
        let callback_permit = if has_callback {
            Some(
                AdmissionPermit::try_acquire(&self.callback_inflight, self.callback_inflight_limit)
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::QueueFull,
                            "the Engine's callback request/event capacity is full",
                        )
                    })?,
            )
        } else {
            None
        };
        let id = RequestId {
            engine: self.id,
            sequence: core.next_sequence,
        };
        core.next_sequence += 1;
        let state = TcpConnectState::new(
            id,
            TcpConnectAdmission {
                connector,
                send_window,
                receive_window,
                read_inactivity_timeout,
                write_inactivity_timeout,
                max_resolve_results: self.max_resolve_results,
                callback,
                occupancy_permit,
                queued_bytes_permit,
                metric_bytes_permit,
                dns_borrow,
                callback_permit,
                metrics: Arc::clone(&self.metrics),
            },
        );
        core.connects.insert(id, Arc::clone(&state));
        let submission = Submission::Connect {
            request,
            state: Arc::clone(&state),
            accepted_at: Instant::now(),
        };
        if !self.queue.try_push(submission) {
            core.connects.remove(&id);
            return Err(Error::new(
                ErrorKind::QueueFull,
                "the Engine's bounded command queue is full",
            ));
        }
        self.metrics.tcp_connect_accepted(
            self.tcp_connections.load(Ordering::Acquire),
            self.tcp_queued_bytes.load(Ordering::Acquire),
        );
        if hostname {
            self.metrics
                .resolution_borrowed(self.resolution_inflight.load(Ordering::Acquire));
        }
        let activation = if has_callback {
            *lock_unpoisoned(&self.callback_activations) += 1;
            Some(CallbackActivation { shared: self })
        } else {
            None
        };
        drop(core);
        if has_callback {
            #[cfg(test)]
            self.run_callback_activation_hook();
            if let Some(job) = state.activate_callback() {
                let _queued = self.callback_domain.enqueue_terminal(job);
            }
        }
        drop(activation);
        Ok(AcceptedTcpConnect { state })
    }

    #[cfg(feature = "resolver")]
    pub(crate) fn accept_resolve(
        self: &Arc<Self>,
        request: ResolveRequest,
        callback: Option<ResolveCallback>,
        max_resolve_results: usize,
    ) -> Result<AcceptedResolve, Error> {
        let has_callback = callback.is_some();
        let mut core = lock_unpoisoned(&self.core);
        if core.lifecycle != LifecycleState::Running {
            return Err(Error::new(
                ErrorKind::EngineStopped,
                "the owning Engine has stopped accepting work",
            ));
        }
        if let Some(max_results) = request.max_results() {
            if max_results > max_resolve_results {
                return Err(Error::limit(
                    LimitKind::ResolveResults,
                    format!(
                        "resolution max_results exceeds the Engine ceiling of {max_resolve_results}"
                    ),
                ));
            }
        }
        if !self.native_resolver_supported() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "public hostname resolution is not available on this Engine",
            ));
        }
        if core.next_sequence == u64::MAX {
            return Err(Error::new(
                ErrorKind::Internal,
                "request identity space is exhausted",
            ));
        }

        let inflight_permit =
            AdmissionPermit::try_acquire(&self.resolution_inflight, self.resolution_inflight_limit)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::QueueFull,
                        "the Engine's accepted/inflight public-resolution capacity is full",
                    )
                })?;
        let callback_permit = if has_callback {
            Some(
                AdmissionPermit::try_acquire(&self.callback_inflight, self.callback_inflight_limit)
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::QueueFull,
                            "the Engine's callback request/event capacity is full",
                        )
                    })?,
            )
        } else {
            None
        };
        let id = RequestId {
            engine: self.id,
            sequence: core.next_sequence,
        };
        core.next_sequence += 1;
        let state = ResolveState::new(
            id,
            callback,
            inflight_permit,
            callback_permit,
            Arc::clone(&self.metrics),
        );
        core.resolutions.insert(id, Arc::clone(&state));

        let effective_max_results = request.max_results().unwrap_or(max_resolve_results);
        let submission = Submission::Resolve {
            request,
            state: Arc::clone(&state),
            accepted_at: Instant::now(),
            max_results: effective_max_results,
        };
        if !self.queue.try_push(submission) {
            core.resolutions.remove(&id);
            return Err(Error::new(
                ErrorKind::QueueFull,
                "the Engine's bounded command queue is full",
            ));
        }
        self.metrics
            .resolution_accepted(self.resolution_inflight.load(Ordering::Acquire));

        let activation = if has_callback {
            *lock_unpoisoned(&self.callback_activations) += 1;
            Some(CallbackActivation { shared: self })
        } else {
            None
        };
        drop(core);

        if has_callback {
            #[cfg(test)]
            self.run_callback_activation_hook();
            if let Some(job) = state.activate_callback() {
                let _queued = self.callback_domain.enqueue_terminal(job);
            }
        }
        drop(activation);
        Ok(AcceptedResolve { state })
    }

    pub(crate) fn accept_stream(
        self: &Arc<Self>,
        request: StreamRequest,
    ) -> Result<ResponseReader, Error> {
        if !self.streaming_supported {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "the selected backend does not support streaming requests",
            ));
        }

        let mut core = lock_unpoisoned(&self.core);
        if core.lifecycle != LifecycleState::Running {
            return Err(Error::new(
                ErrorKind::EngineStopped,
                "the owning Engine has stopped accepting work",
            ));
        }
        if core.next_sequence == u64::MAX {
            return Err(Error::new(
                ErrorKind::Internal,
                "request identity space is exhausted",
            ));
        }
        request.validate()?;
        self.validate_request_limits(request.request())?;
        let response_window = self.max_stream_queue_bytes_per_request;
        if response_window == 0 {
            return Err(Error::limit(
                LimitKind::StreamingQueueBytes,
                "streaming is disabled by the Engine's zero-byte queue window",
            ));
        }
        let upload_window = request.upload_queue_capacity();
        if upload_window > response_window {
            return Err(Error::limit(
                LimitKind::StreamingQueueBytes,
                format!(
                    "streamed upload queue exceeds the configured {response_window} byte per-transfer limit"
                ),
            ));
        }
        let reserved_bytes = response_window.checked_add(upload_window).ok_or_else(|| {
            Error::limit(
                LimitKind::StreamingQueueBytes,
                "streaming queue reservation overflowed",
            )
        })?;
        let inflight_permit = AdmissionPermit::try_acquire(&self.inflight, self.inflight_limit)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::QueueFull,
                    "the Engine's accepted/inflight request capacity is full",
                )
            })?;
        let queued_bytes_permit = BytePermit::try_acquire(
            &self.stream_queued_bytes,
            self.max_stream_queued_bytes,
            reserved_bytes,
        )
        .ok_or_else(|| {
            Error::limit(
                LimitKind::StreamingQueueBytes,
                format!(
                    "streaming queues exceed the Engine's configured {} byte aggregate budget",
                    self.max_stream_queued_bytes
                ),
            )
        })?;
        let parent_queued_bytes_permit = BytePermit::try_acquire(
            &self.queued_bytes,
            self.max_queued_bytes,
            reserved_bytes,
        )
        .ok_or_else(|| {
            Error::limit(
                LimitKind::StreamingQueueBytes,
                format!(
                    "streaming queues exceed the Engine's configured {} byte shared queue budget",
                    self.max_queued_bytes
                ),
            )
        })?;

        let id = RequestId {
            engine: self.id,
            sequence: core.next_sequence,
        };
        core.next_sequence += 1;
        let weak = Arc::downgrade(self);
        let stream_waker: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.queue.wake();
            }
        });
        request.bind_upload(
            response_window,
            self.max_request_body_bytes,
            self.run_mode,
            Arc::clone(&stream_waker),
        )?;
        let handle = RequestHandle::new(Client::new(Arc::clone(self)), id);
        let (reader, response, control) = response_pair(
            handle,
            self.run_mode,
            response_window,
            self.max_response_body_bytes,
            Some(stream_waker),
            Some(Arc::clone(&self.metrics)),
        )?;
        let state = StreamRequestState::new(
            id,
            control,
            inflight_permit,
            queued_bytes_permit,
            parent_queued_bytes_permit,
        );
        core.stream_requests.insert(id, Arc::clone(&state));
        let submission = Submission::Stream {
            request,
            state,
            response,
            accepted_at: Instant::now(),
        };
        if !self.queue.try_push(submission) {
            core.stream_requests.remove(&id);
            return Err(Error::new(
                ErrorKind::QueueFull,
                "the Engine's bounded command queue is full",
            ));
        }
        self.metrics.request_accepted(
            self.inflight.load(Ordering::Acquire),
            self.stream_queued_bytes.load(Ordering::Acquire),
        );
        drop(core);
        Ok(reader)
    }

    pub(crate) fn cancel(&self, request_id: RequestId) -> Result<(), Error> {
        if request_id.engine != self.id {
            return Err(Error::new(
                ErrorKind::WrongEngine,
                "request ID belongs to another Engine",
            ));
        }
        #[cfg(feature = "resolver")]
        let resolve_state;
        let (state, stream_state, tcp_terminal, live_tcp) = {
            let mut core = lock_unpoisoned(&self.core);
            let connect_state = core.connects.get(&request_id).cloned();
            let tcp_won = connect_state.as_ref().is_some_and(|state| {
                self.complete_tcp_state_locked(&mut core, state, TcpConnectCompletion::Cancelled)
            });
            let live_tcp = if tcp_won {
                None
            } else {
                core.live_tcp.get(&request_id).cloned()
            };
            #[cfg(feature = "resolver")]
            {
                resolve_state = core.resolutions.get(&request_id).cloned();
            }
            (
                core.requests.get(&request_id).cloned(),
                core.stream_requests.get(&request_id).cloned(),
                tcp_won.then_some(connect_state).flatten(),
                live_tcp,
            )
        };
        if let Some(tcp_state) = tcp_terminal {
            self.refresh_tcp_resource_metrics();
            if let Some(job) = tcp_state.publish_terminal() {
                let _queued = self.callback_domain.enqueue_terminal(job);
            }
        } else if let Some(io) = live_tcp {
            io.abort(TcpAbort::Cancelled);
        } else if let Some(state) = state {
            self.complete_state(&state, Completion::Cancelled);
        } else if let Some(state) = stream_state {
            self.cancel_stream_state(&state);
        }
        #[cfg(feature = "resolver")]
        if let Some(state) = resolve_state {
            self.complete_resolve_state(&state, ResolveCompletion::Cancelled);
        }
        self.queue.wake();
        Ok(())
    }

    pub(crate) fn cancel_all(&self) {
        #[cfg(feature = "resolver")]
        let resolutions;
        let (requests, stream_requests, tcp_terminals, live_tcp) = {
            let mut core = lock_unpoisoned(&self.core);
            let barrier = core.next_sequence.saturating_sub(1);
            let connects = core
                .connects
                .iter()
                .filter(|(id, _state)| id.sequence <= barrier)
                .map(|(_id, state)| Arc::clone(state))
                .collect::<Vec<_>>();
            let mut tcp_terminals = Vec::new();
            for state in connects {
                let won = self.complete_tcp_state_locked(
                    &mut core,
                    &state,
                    TcpConnectCompletion::Cancelled,
                );
                if won {
                    tcp_terminals.push(state);
                }
            }
            #[cfg(feature = "resolver")]
            {
                resolutions = core
                    .resolutions
                    .iter()
                    .filter(|(id, _state)| id.sequence <= barrier)
                    .map(|(_id, state)| Arc::clone(state))
                    .collect::<Vec<_>>();
            }
            (
                core.requests
                    .iter()
                    .filter(|(id, _state)| id.sequence <= barrier)
                    .map(|(_id, state)| Arc::clone(state))
                    .collect::<Vec<_>>(),
                core.stream_requests
                    .iter()
                    .filter(|(id, _state)| id.sequence <= barrier)
                    .map(|(_id, state)| Arc::clone(state))
                    .collect::<Vec<_>>(),
                tcp_terminals,
                core.live_tcp
                    .iter()
                    .filter(|(id, _state)| id.sequence <= barrier)
                    .map(|(_id, io)| Arc::clone(io))
                    .collect::<Vec<_>>(),
            )
        };
        for state in requests {
            self.complete_state(&state, Completion::Cancelled);
        }
        for state in stream_requests {
            self.cancel_stream_state(&state);
        }
        #[cfg(feature = "resolver")]
        for state in resolutions {
            self.complete_resolve_state(&state, ResolveCompletion::Cancelled);
        }
        if !tcp_terminals.is_empty() {
            self.refresh_tcp_resource_metrics();
        }
        let tcp_jobs = tcp_terminals
            .into_iter()
            .filter_map(|state| state.publish_terminal())
            .collect::<Vec<_>>();
        for job in tcp_jobs {
            let _queued = self.callback_domain.enqueue_terminal(job);
        }
        for io in live_tcp {
            io.abort(TcpAbort::Cancelled);
        }
        self.queue.wake();
    }

    pub(crate) fn begin_shutdown(&self) {
        #[cfg(feature = "resolver")]
        let resolutions;
        let (requests, stream_requests, tcp_terminals, live_tcp) = {
            let mut core = lock_unpoisoned(&self.core);
            if core.lifecycle == LifecycleState::Running {
                core.lifecycle = LifecycleState::ShuttingDown;
            }
            self.stopped.store(true, Ordering::Release);
            let connects = core.connects.values().cloned().collect::<Vec<_>>();
            let mut tcp_terminals = Vec::new();
            for state in connects {
                let won = self.complete_tcp_state_locked(
                    &mut core,
                    &state,
                    TcpConnectCompletion::Failed(Error::new(
                        ErrorKind::EngineStopped,
                        "the owning Engine stopped during TCP connection establishment",
                    )),
                );
                if won {
                    tcp_terminals.push(state);
                }
            }
            #[cfg(feature = "resolver")]
            {
                resolutions = core.resolutions.values().cloned().collect::<Vec<_>>();
            }
            (
                core.requests.values().cloned().collect::<Vec<_>>(),
                core.stream_requests.values().cloned().collect::<Vec<_>>(),
                tcp_terminals,
                core.live_tcp.values().cloned().collect::<Vec<_>>(),
            )
        };
        for state in requests {
            self.complete_state(&state, Completion::Cancelled);
        }
        for state in stream_requests {
            self.cancel_stream_state(&state);
        }
        #[cfg(feature = "resolver")]
        for state in resolutions {
            self.complete_resolve_state(&state, ResolveCompletion::Cancelled);
        }
        if !tcp_terminals.is_empty() {
            self.refresh_tcp_resource_metrics();
        }
        let tcp_jobs = tcp_terminals
            .into_iter()
            .filter_map(|state| state.publish_terminal())
            .collect::<Vec<_>>();
        for job in tcp_jobs {
            let _queued = self.callback_domain.enqueue_terminal(job);
        }
        for io in live_tcp {
            io.abort(TcpAbort::EngineStopped);
        }
        self.queue.wake();
    }

    pub(crate) fn mark_stopped(&self) {
        lock_unpoisoned(&self.core).lifecycle = LifecycleState::Stopped;
        self.stopped.store(true, Ordering::Release);
        self.queue.wake();
    }

    pub(crate) fn try_begin_callback_event(
        &self,
    ) -> Result<(AdmissionPermit, CallbackActivation<'_>), Error> {
        let core = lock_unpoisoned(&self.core);
        if core.lifecycle != LifecycleState::Running {
            return Err(Error::new(
                ErrorKind::EngineStopped,
                "the owning Engine has stopped accepting work",
            ));
        }
        let permit =
            AdmissionPermit::try_acquire(&self.callback_inflight, self.callback_inflight_limit)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::QueueFull,
                        "the Engine's callback request/event capacity is full",
                    )
                })?;
        *lock_unpoisoned(&self.callback_activations) += 1;
        drop(core);
        Ok((permit, CallbackActivation { shared: self }))
    }

    pub(crate) fn enqueue_callback_job(&self, job: CallbackJob) {
        let _queued = self.callback_domain.enqueue_terminal(job);
    }

    #[cfg(test)]
    pub(crate) fn fire_callback_activation_hook(&self) {
        self.run_callback_activation_hook();
    }

    pub(crate) fn wait_for_callback_activations(&self) {
        let activations = lock_unpoisoned(&self.callback_activations);
        let result = self
            .callback_activations_done
            .wait_while(activations, |activations| *activations != 0)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        drop(result);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn complete_id(&self, id: RequestId, completion: Completion) -> bool {
        if id.engine != self.id {
            return false;
        }
        let state = lock_unpoisoned(&self.core).requests.get(&id).cloned();
        match state {
            Some(state) => self.complete_state(&state, completion),
            None => false,
        }
    }

    #[cfg(feature = "resolver")]
    pub(crate) fn complete_resolve_state(
        &self,
        state: &Arc<ResolveState>,
        completion: ResolveCompletion,
    ) -> bool {
        let (won, job) = state.commit(completion);
        if !won {
            return false;
        }
        lock_unpoisoned(&self.core).resolutions.remove(&state.id());
        if let Some(job) = job {
            let _queued = self.callback_domain.enqueue_terminal(job);
        }
        true
    }

    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) fn complete_tcp_connected(
        self: &Arc<Self>,
        state: &Arc<TcpConnectState>,
        local: std::net::SocketAddr,
        peer: std::net::SocketAddr,
    ) -> Option<TcpIoOwner> {
        let mut core = lock_unpoisoned(&self.core);
        if core.lifecycle != LifecycleState::Running {
            return None;
        }
        if !core
            .connects
            .get(&state.id())
            .is_some_and(|current| Arc::ptr_eq(current, state))
        {
            return None;
        }
        let mut inner = lock_unpoisoned(&state.inner);
        if inner.terminal {
            return None;
        }
        let occupancy_permit = inner
            .occupancy_permit
            .take()
            .expect("accepted TCP connect owns its occupancy permit");
        let queued_bytes_permit = inner
            .queued_bytes_permit
            .take()
            .expect("accepted TCP connect owns its queue permit");
        let metric_bytes_permit = inner
            .metric_bytes_permit
            .take()
            .expect("accepted TCP connect owns its metric queue permit");
        debug_assert!(
            inner.dns_borrow.is_none(),
            "hostname DNS capacity must be released before socket establishment"
        );
        let weak = Arc::downgrade(self);
        let id = state.id();
        let release = Box::new(move || {
            drop(occupancy_permit);
            drop(queued_bytes_permit);
            drop(metric_bytes_permit);
            if let Some(shared) = weak.upgrade() {
                shared.release_live_tcp(id);
            }
        });
        let wake_weak = Arc::downgrade(self);
        let engine_waker = Arc::new(move || {
            if let Some(shared) = wake_weak.upgrade() {
                shared.queue.wake();
            }
        });
        let (io, owner) = TcpIoShared::pair(TcpIoConfig {
            engine_id: self.id,
            request_id: id,
            shared: Arc::clone(self),
            run_mode: self.run_mode,
            send_window: state.send_window,
            receive_window: state.receive_window,
            local,
            peer,
            engine_waker: Some(engine_waker),
            on_release: release,
        });
        let handle = TcpConnectionHandle::new(state.connector.clone(), id);
        let completion =
            TcpConnectCompletion::Completed(TcpConnection::from_shared(Arc::clone(&io), handle));
        state.metrics.tcp_connect_terminal(&completion);
        inner.terminal = true;
        inner.completion = Some(completion);
        inner.delivery_ready = true;
        state.changed.notify_all();
        let job = TcpConnectState::take_terminal_job(id, &mut inner);
        core.connects.remove(&id);
        core.live_tcp.insert(id, io);
        drop(inner);
        drop(core);
        if let Some(job) = job {
            let _queued = self.callback_domain.enqueue_terminal(job);
        }
        Some(owner)
    }

    pub(crate) fn complete_tcp_state(
        &self,
        state: &Arc<TcpConnectState>,
        completion: TcpConnectCompletion,
    ) -> bool {
        let mut core = lock_unpoisoned(&self.core);
        let won = self.complete_tcp_state_locked(&mut core, state, completion);
        drop(core);
        if !won {
            return false;
        }
        self.refresh_tcp_resource_metrics();
        if let Some(job) = state.publish_terminal() {
            let _queued = self.callback_domain.enqueue_terminal(job);
        }
        true
    }

    fn complete_tcp_state_locked(
        &self,
        core: &mut CoreState,
        state: &Arc<TcpConnectState>,
        completion: TcpConnectCompletion,
    ) -> bool {
        if !core
            .connects
            .get(&state.id())
            .is_some_and(|current| Arc::ptr_eq(current, state))
        {
            return false;
        }
        let won = state.commit_terminal(completion);
        if !won {
            return false;
        }
        #[cfg(test)]
        self.run_tcp_terminal_commit_hook();
        core.connects.remove(&state.id());
        true
    }

    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    fn release_live_tcp(&self, id: RequestId) {
        lock_unpoisoned(&self.core).live_tcp.remove(&id);
        self.refresh_tcp_resource_metrics();
        self.queue.wake();
    }

    fn refresh_tcp_resource_metrics(&self) {
        self.metrics.set_tcp_resources(
            self.tcp_connections.load(Ordering::Acquire),
            self.tcp_queued_bytes.load(Ordering::Acquire),
        );
    }

    pub(crate) fn complete_state(&self, state: &Arc<RequestState>, completion: Completion) -> bool {
        let (won, job) = state.commit(completion);
        if !won {
            return false;
        }
        lock_unpoisoned(&self.core).requests.remove(&state.id());
        if let Some(job) = job {
            let _queued = self.callback_domain.enqueue_terminal(job);
        }
        true
    }

    pub(crate) fn finish_stream_state(&self, state: &Arc<StreamRequestState>) {
        state.release_permits();
        let removed = lock_unpoisoned(&self.core)
            .stream_requests
            .remove(&state.id())
            .is_some();
        debug_assert!(!removed || state.control.is_terminal());
    }

    fn cancel_stream_state(&self, state: &Arc<StreamRequestState>) -> bool {
        let won = state.cancel();
        if won {
            self.finish_stream_state(state);
        }
        won
    }

    pub(crate) fn fail_all(&self, error: Error) {
        #[cfg(feature = "resolver")]
        let resolutions;
        let (requests, stream_requests, tcp_terminals, live_tcp) = {
            let mut core = lock_unpoisoned(&self.core);
            let connects = core.connects.values().cloned().collect::<Vec<_>>();
            let mut tcp_terminals = Vec::new();
            for state in connects {
                let won = self.complete_tcp_state_locked(
                    &mut core,
                    &state,
                    TcpConnectCompletion::Failed(error.clone()),
                );
                if won {
                    tcp_terminals.push(state);
                }
            }
            #[cfg(feature = "resolver")]
            {
                resolutions = core.resolutions.values().cloned().collect::<Vec<_>>();
            }
            (
                core.requests.values().cloned().collect::<Vec<_>>(),
                core.stream_requests.values().cloned().collect::<Vec<_>>(),
                tcp_terminals,
                core.live_tcp.values().cloned().collect::<Vec<_>>(),
            )
        };
        for state in requests {
            self.complete_state(&state, Completion::Failed(error.clone()));
        }
        for state in stream_requests {
            if state.fail(error.clone()) {
                self.finish_stream_state(&state);
            }
        }
        #[cfg(feature = "resolver")]
        for state in resolutions {
            self.complete_resolve_state(&state, ResolveCompletion::Failed(error.clone()));
        }
        if !tcp_terminals.is_empty() {
            self.refresh_tcp_resource_metrics();
        }
        let tcp_jobs = tcp_terminals
            .into_iter()
            .filter_map(|state| state.publish_terminal())
            .collect::<Vec<_>>();
        for job in tcp_jobs {
            let _queued = self.callback_domain.enqueue_terminal(job);
        }
        for io in live_tcp {
            io.abort(TcpAbort::Failed(error.clone()));
        }
        self.queue.wake();
    }

    pub(crate) fn metrics_snapshot(&self) -> EngineMetrics {
        self.metrics.snapshot(
            self.inflight.load(Ordering::Acquire),
            self.stream_queued_bytes.load(Ordering::Acquire),
            self.resolution_inflight.load(Ordering::Acquire),
        )
    }

    fn validate_request_limits(&self, request: &Request) -> Result<(), Error> {
        if request.body().len() > self.max_request_body_bytes {
            return Err(Error::limit(
                LimitKind::RequestBodyBytes,
                format!(
                    "request body exceeds the configured {} byte limit",
                    self.max_request_body_bytes
                ),
            ));
        }
        if request.headers().len() > self.max_header_count {
            return Err(Error::limit(
                LimitKind::RequestHeaderCount,
                format!(
                    "request headers exceed the configured {} field limit",
                    self.max_header_count
                ),
            ));
        }
        let header_bytes = request.headers().iter().try_fold(0_usize, |total, header| {
            total
                .checked_add(header.name().len())?
                .checked_add(header.value().len())?
                .checked_add(4)
        });
        if header_bytes.is_none_or(|bytes| bytes > self.max_header_bytes) {
            return Err(Error::limit(
                LimitKind::RequestHeaderBytes,
                format!(
                    "request headers exceed the configured {} byte limit",
                    self.max_header_bytes
                ),
            ));
        }
        Ok(())
    }

    fn finish_callback_activation(&self) {
        let mut activations = lock_unpoisoned(&self.callback_activations);
        *activations -= 1;
        if *activations == 0 {
            self.callback_activations_done.notify_all();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn active_count(&self) -> usize {
        let core = lock_unpoisoned(&self.core);
        #[cfg(feature = "resolver")]
        let resolutions = core.resolutions.len();
        #[cfg(not(feature = "resolver"))]
        let resolutions = 0;
        core.requests.len()
            + core.stream_requests.len()
            + resolutions
            + core.connects.len()
            + core.live_tcp.len()
    }

    #[cfg(test)]
    pub(crate) fn set_callback_activation_hook(&self, hook: impl FnOnce() + Send + 'static) {
        *lock_unpoisoned(&self.callback_activation_hook) = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_callback_activation_hook(&self) {
        if let Some(hook) = lock_unpoisoned(&self.callback_activation_hook).take() {
            hook();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_tcp_terminal_commit_hook(&self, hook: impl FnOnce() + Send + 'static) {
        *lock_unpoisoned(&self.tcp_terminal_commit_hook) = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_tcp_terminal_commit_hook(&self) {
        if let Some(hook) = lock_unpoisoned(&self.tcp_terminal_commit_hook).take() {
            hook();
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use crate::{Completion, EngineConfig, Error, ErrorKind, Request, TcpConnectRequest};

    #[test]
    fn hostname_tcp_borrows_resolution_capacity_without_counting_a_resolver_operation() {
        let config = EngineConfig::manual().with_max_inflight_resolutions(
            std::num::NonZeroUsize::new(1).expect("nonzero resolution capacity"),
        );
        let (engine, _controller) = crate::testing::engine(config).expect("Engine must construct");
        let shared = engine.shared_for_testing();
        shared.set_native_resolver_supported(true);
        shared.set_standalone_tcp_supported(true);

        let pending = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::hostname("host.test", 1234)
                    .build()
                    .expect("hostname request must build"),
            )
            .expect("hostname connect must be admitted");
        let metrics = engine.metrics();
        assert_eq!(metrics.tcp_connects_accepted(), 1);
        assert_eq!(metrics.resolutions_accepted(), 0);
        assert_eq!(metrics.current().inflight_resolutions(), 1);
        assert_eq!(metrics.high_water().inflight_resolutions(), 1);

        pending.handle().cancel().expect("connect cancel must win");
        assert!(matches!(pending.wait(), TcpConnectCompletion::Cancelled));
        let released = engine.metrics();
        assert_eq!(released.current().inflight_resolutions(), 0);
        assert_eq!(released.current().standalone_tcp_connections(), 0);
        assert_eq!(released.resolutions_cancelled(), 0);
        engine.shutdown().expect("Engine must stop");
    }

    #[test]
    #[cfg(feature = "resolver")]
    fn full_resolution_capacity_rejects_hostname_before_tcp_acceptance_but_not_literal() {
        let config = EngineConfig::manual().with_max_inflight_resolutions(
            std::num::NonZeroUsize::new(1).expect("nonzero resolution capacity"),
        );
        let (engine, _controller) = crate::testing::engine(config).expect("Engine must construct");
        let shared = engine.shared_for_testing();
        shared.set_native_resolver_supported(true);
        shared.set_standalone_tcp_supported(true);

        let resolve = engine
            .resolver()
            .submit(
                crate::ResolveRequest::hostname("held.test")
                    .build()
                    .expect("resolve request must build"),
            )
            .expect("public resolve must occupy the only permit");
        let before = engine.metrics();
        let error = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::hostname("host.test", 1234)
                    .build()
                    .expect("hostname request must build"),
            )
            .expect_err("hostname connect must respect public DNS capacity");
        assert_eq!(error.kind(), ErrorKind::QueueFull);
        let after = engine.metrics();
        assert_eq!(
            after.tcp_connects_accepted(),
            before.tcp_connects_accepted()
        );
        assert_eq!(after.current().standalone_tcp_connections(), 0);
        assert_eq!(after.current().reserved_tcp_queue_bytes(), 0);

        let literal = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::literal(SocketAddr::from((Ipv4Addr::LOCALHOST, 9)))
                    .build()
                    .expect("literal request must build"),
            )
            .expect("literal connect must not consume DNS capacity");
        literal.handle().cancel().expect("literal cancel must win");
        assert!(matches!(literal.wait(), TcpConnectCompletion::Cancelled));
        resolve.handle().cancel().expect("resolve cancel must win");
        assert!(matches!(
            resolve.wait(),
            crate::ResolveCompletion::Cancelled
        ));
        engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn tcp_cancel_holds_the_registry_transition_until_terminal_commit_is_complete() {
        let (engine, _controller) =
            crate::testing::engine(EngineConfig::manual()).expect("Engine must construct");
        let shared = engine.shared_for_testing();
        shared.set_standalone_tcp_supported(true);
        let pending = engine
            .tcp_connector()
            .submit(
                TcpConnectRequest::literal(SocketAddr::from((Ipv4Addr::LOCALHOST, 9)))
                    .build()
                    .expect("literal connect must build"),
            )
            .expect("literal connect must be admitted");
        let state = shared
            .queue
            .drain()
            .into_iter()
            .find_map(|submission| match submission {
                Submission::Connect { state, .. } => Some(state),
                _ => None,
            })
            .expect("connect submission must be queued");

        let (committed_tx, committed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        shared.set_tcp_terminal_commit_hook(move || {
            committed_tx
                .send(())
                .expect("test must observe terminal commit");
            release_rx
                .recv()
                .expect("test must release terminal transition");
        });

        let handle = pending.handle();
        let cancel_thread = thread::spawn(move || handle.cancel());
        committed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancel must reach its terminal commit");

        let success_shared = Arc::clone(&shared);
        let success_state = Arc::clone(&state);
        let (success_tx, success_rx) = mpsc::channel();
        let success_thread = thread::spawn(move || {
            let result = success_shared.complete_tcp_connected(
                &success_state,
                SocketAddr::from((Ipv4Addr::LOCALHOST, 40_000)),
                SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
            );
            success_tx
                .send(result.is_none())
                .expect("test must observe success transition");
        });

        assert!(
            success_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "success must remain serialized behind the cancel transition's registry lock"
        );
        release_tx.send(()).expect("release terminal transition");
        cancel_thread
            .join()
            .expect("cancel thread must join")
            .expect("cancel must succeed");
        assert!(
            success_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("success transition must finish after cancellation")
        );
        success_thread.join().expect("success thread must join");
        assert!(matches!(pending.wait(), TcpConnectCompletion::Cancelled));
        engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn user_callback_observes_registry_unlocked() {
        let (engine, controller) =
            crate::testing::engine(EngineConfig::spawned()).expect("Engine must construct");
        let shared = engine.shared_for_testing();
        let client = engine.client();
        let (result_tx, result_rx) = mpsc::channel();
        let request = Request::get("https://example.invalid/")
            .build()
            .expect("test request must build");

        let handle = client
            .start(request, move |_completion| {
                result_tx
                    .send(shared.core.try_lock().is_ok())
                    .expect("test receiver must remain");
            })
            .expect("request must submit");
        assert!(controller.complete(handle.id(), Completion::Cancelled));
        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("callback must run")
        );
        engine.shutdown().expect("Engine must stop");
    }

    #[test]
    fn external_wakeup_failure_is_latched_for_the_reactor() {
        let (engine, _controller) =
            crate::testing::engine(EngineConfig::manual()).expect("Engine must construct");
        let shared = engine.shared_for_testing();
        shared.queue.set_external_waker(Some(Arc::new(|| {
            Err(Error::new(
                ErrorKind::Internal,
                "deliberate external wake failure",
            ))
        })));

        shared.queue.wake();
        let failure = shared
            .queue
            .take_external_wake_failure()
            .expect("wake failure must be retained until the reactor sees it");
        assert_eq!(failure.kind(), ErrorKind::Internal);
        assert_eq!(failure.message(), "deliberate external wake failure");
        engine.shutdown().expect("Engine must stop");
    }
}
