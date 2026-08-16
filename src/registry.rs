use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

use crate::context;
use crate::dispatch::{CallbackDomain, CallbackJob};
use crate::{Completion, EngineConfig, Error, ErrorKind, LimitKind, Request, RequestId, RunMode};

pub(crate) type CompletionCallback = Box<dyn FnOnce(Completion) + Send + 'static>;
type ExternalWaker = Arc<dyn Fn() -> Result<(), Error> + Send + Sync + 'static>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleState {
    Running,
    ShuttingDown,
    Stopped,
}

struct AdmissionPermit {
    inflight: Arc<AtomicUsize>,
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl AdmissionPermit {
    fn try_acquire(inflight: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        inflight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
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

pub(crate) struct Submission {
    pub(crate) request: Request,
    pub(crate) state: Arc<RequestState>,
    pub(crate) accepted_at: Instant,
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
}

impl CommandQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(QueueState {
                submissions: VecDeque::new(),
                generation: 0,
            }),
            changed: Condvar::new(),
            external_waker: Mutex::new(None),
            external_wake_failure: Mutex::new(None),
        }
    }

    fn try_push(&self, submission: Submission) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        if state.submissions.len() >= self.capacity {
            return false;
        }
        state.submissions.push_back(submission);
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_one();
        drop(state);
        self.wake_external();
        true
    }

    pub(crate) fn drain(&self) -> Vec<Submission> {
        lock_unpoisoned(&self.state).submissions.drain(..).collect()
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
}

pub(crate) struct AcceptedRequest {
    pub(crate) state: Arc<RequestState>,
}

pub(crate) struct Shared {
    pub(crate) id: u64,
    pub(crate) stopped: AtomicBool,
    pub(crate) run_mode: RunMode,
    pub(crate) queue: CommandQueue,
    callback_domain: Arc<CallbackDomain>,
    core: Mutex<CoreState>,
    inflight: Arc<AtomicUsize>,
    inflight_limit: usize,
    callback_inflight: Arc<AtomicUsize>,
    callback_inflight_limit: usize,
    max_request_body_bytes: usize,
    max_header_bytes: usize,
    max_header_count: usize,
    callback_activations: Mutex<usize>,
    callback_activations_done: Condvar,
    #[cfg(test)]
    callback_activation_hook: Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
}

struct CallbackActivation<'shared> {
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
    ) -> Arc<Self> {
        let command_capacity = config.command_queue_capacity().get();
        let callback_capacity = config.callback_queue_capacity().get();
        Arc::new(Self {
            id,
            stopped: AtomicBool::new(false),
            run_mode: config.run_mode(),
            queue: CommandQueue::new(command_capacity),
            callback_domain,
            core: Mutex::new(CoreState {
                lifecycle: LifecycleState::Running,
                next_sequence: 1,
                requests: HashMap::new(),
            }),
            inflight: Arc::new(AtomicUsize::new(0)),
            inflight_limit: config.max_inflight_requests().get(),
            callback_inflight: Arc::new(AtomicUsize::new(0)),
            callback_inflight_limit: callback_capacity,
            max_request_body_bytes: config.max_request_body_bytes(),
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
        let state = RequestState::new(id, callback, inflight_permit, callback_permit);
        core.requests.insert(id, Arc::clone(&state));

        let submission = Submission {
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

    pub(crate) fn cancel(&self, request_id: RequestId) -> Result<(), Error> {
        if request_id.engine != self.id {
            return Err(Error::new(
                ErrorKind::WrongEngine,
                "request ID belongs to another Engine",
            ));
        }
        let state = lock_unpoisoned(&self.core)
            .requests
            .get(&request_id)
            .cloned();
        if let Some(state) = state {
            self.complete_state(&state, Completion::Cancelled);
        }
        self.queue.wake();
        Ok(())
    }

    pub(crate) fn cancel_all(&self) {
        let requests = {
            let core = lock_unpoisoned(&self.core);
            let barrier = core.next_sequence.saturating_sub(1);
            core.requests
                .iter()
                .filter(|(id, _state)| id.sequence <= barrier)
                .map(|(_id, state)| Arc::clone(state))
                .collect::<Vec<_>>()
        };
        for state in requests {
            self.complete_state(&state, Completion::Cancelled);
        }
        self.queue.wake();
    }

    pub(crate) fn begin_shutdown(&self) {
        let requests = {
            let mut core = lock_unpoisoned(&self.core);
            if core.lifecycle == LifecycleState::Running {
                core.lifecycle = LifecycleState::ShuttingDown;
            }
            self.stopped.store(true, Ordering::Release);
            core.requests.values().cloned().collect::<Vec<_>>()
        };
        for state in requests {
            self.complete_state(&state, Completion::Cancelled);
        }
        self.queue.wake();
    }

    pub(crate) fn mark_stopped(&self) {
        lock_unpoisoned(&self.core).lifecycle = LifecycleState::Stopped;
        self.stopped.store(true, Ordering::Release);
        self.queue.wake();
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

    pub(crate) fn fail_all(&self, error: Error) {
        let requests = lock_unpoisoned(&self.core)
            .requests
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for state in requests {
            self.complete_state(&state, Completion::Failed(error.clone()));
        }
        self.queue.wake();
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
        lock_unpoisoned(&self.core).requests.len()
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
