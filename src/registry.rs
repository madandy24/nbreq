use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

use crate::atomic::try_update_usize;
use crate::context;
use crate::dispatch::{CallbackDomain, CallbackJob};
use crate::metrics::{EngineMetrics, Metrics};
use crate::stream::{ResponseControl, ResponseSink, response_pair};
use crate::{
    Client, Completion, EngineConfig, Error, ErrorKind, LimitKind, Request, RequestHandle,
    RequestId, ResolveCompletion, ResolveRequest, ResponseReader, RunMode, StreamRequest,
};

pub(crate) type CompletionCallback = Box<dyn FnOnce(Completion) + Send + 'static>;
pub(crate) type ResolveCallback = Box<dyn FnOnce(ResolveCompletion) + Send + 'static>;
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

struct BytePermit {
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

struct ResolveInner {
    completion: Option<ResolveCompletion>,
    callback: Option<ResolveCallback>,
    callback_active: bool,
    inflight_permit: Option<AdmissionPermit>,
    callback_permit: Option<AdmissionPermit>,
}

pub(crate) struct ResolveState {
    id: RequestId,
    inner: Mutex<ResolveInner>,
    changed: Condvar,
    metrics: Arc<Metrics>,
}

impl fmt::Debug for ResolveState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolveState")
            .field("id", &self.id)
            .field("terminal", &self.is_terminal())
            .finish()
    }
}

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

pub(crate) struct StreamRequestState {
    id: RequestId,
    control: ResponseControl,
    permits: Mutex<Option<(AdmissionPermit, BytePermit)>>,
}

impl StreamRequestState {
    fn new(
        id: RequestId,
        control: ResponseControl,
        inflight_permit: AdmissionPermit,
        queued_bytes_permit: BytePermit,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            control,
            permits: Mutex::new(Some((inflight_permit, queued_bytes_permit))),
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
    Resolve {
        request: ResolveRequest,
        state: Arc<ResolveState>,
        accepted_at: Instant,
        max_results: usize,
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
    resolutions: HashMap<RequestId, Arc<ResolveState>>,
}

pub(crate) struct AcceptedRequest {
    pub(crate) state: Arc<RequestState>,
}

pub(crate) struct AcceptedResolve {
    pub(crate) state: Arc<ResolveState>,
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
    public_resolver_supported: AtomicBool,
    callback_inflight: Arc<AtomicUsize>,
    callback_inflight_limit: usize,
    streaming_supported: bool,
    max_request_body_bytes: usize,
    max_response_body_bytes: usize,
    max_stream_queue_bytes_per_request: usize,
    stream_queued_bytes: Arc<AtomicUsize>,
    max_stream_queued_bytes: usize,
    max_header_bytes: usize,
    max_header_count: usize,
    callback_activations: Mutex<usize>,
    callback_activations_done: Condvar,
    #[cfg(test)]
    callback_activation_hook: Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
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
                resolutions: HashMap::new(),
            }),
            inflight: Arc::new(AtomicUsize::new(0)),
            inflight_limit: config.max_inflight_requests().get(),
            resolution_inflight: Arc::new(AtomicUsize::new(0)),
            resolution_inflight_limit: config.max_inflight_resolutions().get(),
            public_resolver_supported: AtomicBool::new(false),
            callback_inflight: Arc::new(AtomicUsize::new(0)),
            callback_inflight_limit: callback_capacity,
            streaming_supported,
            max_request_body_bytes: config.max_request_body_bytes(),
            max_response_body_bytes: config.max_response_body_bytes(),
            max_stream_queue_bytes_per_request: config.max_stream_queue_bytes_per_request(),
            stream_queued_bytes: Arc::new(AtomicUsize::new(0)),
            max_stream_queued_bytes: config.max_stream_queued_bytes(),
            max_header_bytes: config.max_header_bytes(),
            max_header_count: config.max_header_count(),
            callback_activations: Mutex::new(0),
            callback_activations_done: Condvar::new(),
            #[cfg(test)]
            callback_activation_hook: Mutex::new(None),
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

    pub(crate) fn set_public_resolver_supported(&self, supported: bool) {
        self.public_resolver_supported
            .store(supported, Ordering::Release);
    }

    pub(crate) fn public_resolver_supported(&self) -> bool {
        self.public_resolver_supported.load(Ordering::Acquire)
    }

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
        if !self.public_resolver_supported() {
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
        let state = StreamRequestState::new(id, control, inflight_permit, queued_bytes_permit);
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
        let (state, stream_state, resolve_state) = {
            let core = lock_unpoisoned(&self.core);
            (
                core.requests.get(&request_id).cloned(),
                core.stream_requests.get(&request_id).cloned(),
                core.resolutions.get(&request_id).cloned(),
            )
        };
        if let Some(state) = state {
            self.complete_state(&state, Completion::Cancelled);
        } else if let Some(state) = stream_state {
            self.cancel_stream_state(&state);
        } else if let Some(state) = resolve_state {
            self.complete_resolve_state(&state, ResolveCompletion::Cancelled);
        }
        self.queue.wake();
        Ok(())
    }

    pub(crate) fn cancel_all(&self) {
        let (requests, stream_requests, resolutions) = {
            let core = lock_unpoisoned(&self.core);
            let barrier = core.next_sequence.saturating_sub(1);
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
                core.resolutions
                    .iter()
                    .filter(|(id, _state)| id.sequence <= barrier)
                    .map(|(_id, state)| Arc::clone(state))
                    .collect::<Vec<_>>(),
            )
        };
        for state in requests {
            self.complete_state(&state, Completion::Cancelled);
        }
        for state in stream_requests {
            self.cancel_stream_state(&state);
        }
        for state in resolutions {
            self.complete_resolve_state(&state, ResolveCompletion::Cancelled);
        }
        self.queue.wake();
    }

    pub(crate) fn begin_shutdown(&self) {
        let (requests, stream_requests, resolutions) = {
            let mut core = lock_unpoisoned(&self.core);
            if core.lifecycle == LifecycleState::Running {
                core.lifecycle = LifecycleState::ShuttingDown;
            }
            self.stopped.store(true, Ordering::Release);
            (
                core.requests.values().cloned().collect::<Vec<_>>(),
                core.stream_requests.values().cloned().collect::<Vec<_>>(),
                core.resolutions.values().cloned().collect::<Vec<_>>(),
            )
        };
        for state in requests {
            self.complete_state(&state, Completion::Cancelled);
        }
        for state in stream_requests {
            self.cancel_stream_state(&state);
        }
        for state in resolutions {
            self.complete_resolve_state(&state, ResolveCompletion::Cancelled);
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
        let (requests, stream_requests, resolutions) = {
            let core = lock_unpoisoned(&self.core);
            (
                core.requests.values().cloned().collect::<Vec<_>>(),
                core.stream_requests.values().cloned().collect::<Vec<_>>(),
                core.resolutions.values().cloned().collect::<Vec<_>>(),
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
        for state in resolutions {
            self.complete_resolve_state(&state, ResolveCompletion::Failed(error.clone()));
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
        core.requests.len() + core.stream_requests.len() + core.resolutions.len()
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
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use crate::{Completion, EngineConfig, Error, ErrorKind, Request};

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
