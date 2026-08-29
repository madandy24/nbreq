use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Instant;

use crate::backend::BackendFactory;
#[cfg(feature = "resolver")]
use crate::backend::BackendResolveCompletion;
use crate::backend::{Backend, BackendCompletion, PollMode, interruptible_poll_deadline};
#[cfg(feature = "resolver")]
use crate::registry::ResolveState;
use crate::registry::{RequestState, Shared, StreamRequestState, Submission, TcpConnectSink};
use crate::{DriveStatus, Error, ErrorKind, RequestId, ShutdownError};

pub(crate) struct ReactorCore<B: Backend + ?Sized> {
    backend: Box<B>,
    active: HashMap<RequestId, Arc<RequestState>>,
    active_streams: HashMap<RequestId, Arc<StreamRequestState>>,
    #[cfg(feature = "resolver")]
    active_resolves: HashMap<RequestId, Arc<ResolveState>>,
}

impl<B: Backend + ?Sized> ReactorCore<B> {
    pub(crate) fn new(backend: Box<B>) -> Self {
        Self {
            backend,
            active: HashMap::new(),
            active_streams: HashMap::new(),
            #[cfg(feature = "resolver")]
            active_resolves: HashMap::new(),
        }
    }

    pub(crate) fn drive(
        &mut self,
        shared: &Arc<Shared>,
        deadline: Instant,
    ) -> Result<DriveStatus, Error> {
        if let Some(error) = shared.queue.take_external_wake_failure() {
            return Err(error);
        }
        let submissions = shared.queue.drain();
        let mut progressed = !submissions.is_empty();
        for submission in submissions {
            match submission {
                Submission::Buffered {
                    request,
                    state,
                    accepted_at,
                } => {
                    let id = state.id();
                    if state.is_terminal() {
                        continue;
                    }
                    match self.backend.submit(id, request, accepted_at) {
                        Some(completion) => {
                            shared.complete_state(&state, completion);
                        }
                        None => {
                            self.active.insert(id, state);
                        }
                    }
                }
                Submission::Stream {
                    request,
                    state,
                    mut response,
                    accepted_at,
                } => {
                    let id = state.id();
                    if state.is_terminal() {
                        response.cancel();
                        continue;
                    }
                    self.backend
                        .submit_stream(id, request, response, accepted_at);
                    self.active_streams.insert(id, state);
                }
                #[cfg(feature = "resolver")]
                Submission::Resolve {
                    request,
                    state,
                    accepted_at,
                    max_results,
                } => {
                    let id = state.id();
                    if state.is_terminal() {
                        continue;
                    }
                    match self
                        .backend
                        .submit_resolve(id, request, accepted_at, max_results)
                    {
                        Some(completion) => {
                            shared.complete_resolve_state(&state, completion);
                        }
                        None => {
                            self.active_resolves.insert(id, state);
                        }
                    }
                }
                Submission::Connect {
                    request,
                    state,
                    accepted_at,
                } => {
                    if state.is_terminal() {
                        continue;
                    }
                    let sink = TcpConnectSink::new(shared, state);
                    self.backend.submit_tcp_connect(request, sink, accepted_at);
                }
            }
        }

        progressed |= self.reap_cancelled();
        progressed |= self.reap_terminal_streams(shared);
        #[cfg(feature = "resolver")]
        {
            let early_resolves = self.backend.poll_resolves()?;
            progressed |= !early_resolves.is_empty();
            self.commit_resolve_completions(shared, early_resolves);
        }
        let completions = self.backend.poll(deadline)?;
        #[cfg(feature = "resolver")]
        let resolve_completions = self.backend.poll_resolves()?;
        if let Some(error) = shared.queue.take_external_wake_failure() {
            return Err(error);
        }
        progressed |= !completions.is_empty();
        #[cfg(feature = "resolver")]
        {
            progressed |= !resolve_completions.is_empty();
        }
        self.commit_backend_completions(shared, completions);
        #[cfg(feature = "resolver")]
        self.commit_resolve_completions(shared, resolve_completions);
        progressed |= self.reap_cancelled();
        progressed |= self.reap_terminal_streams(shared);

        if progressed {
            Ok(DriveStatus::Progress)
        } else if Instant::now() >= deadline {
            Ok(DriveStatus::DeadlineReached)
        } else {
            Ok(DriveStatus::Idle)
        }
    }

    pub(crate) fn shutdown(&mut self, shared: &Arc<Shared>) -> Result<(), ShutdownError> {
        shared.queue.set_external_waker(None);
        for submission in shared.queue.drain() {
            match submission {
                Submission::Buffered { state, .. } => {
                    if !state.is_terminal() {
                        shared.complete_state(&state, crate::Completion::Cancelled);
                    }
                }
                Submission::Stream {
                    state,
                    mut response,
                    ..
                } => {
                    response.cancel();
                    shared.finish_stream_state(&state);
                }
                #[cfg(feature = "resolver")]
                Submission::Resolve { state, .. } => {
                    if !state.is_terminal() {
                        shared.complete_resolve_state(&state, crate::ResolveCompletion::Cancelled);
                    }
                }
                Submission::Connect { state, .. } => {
                    if !state.is_terminal() {
                        shared.complete_tcp_state(
                            &state,
                            crate::TcpConnectCompletion::Failed(Error::new(
                                ErrorKind::EngineStopped,
                                "the owning Engine stopped during TCP connection establishment",
                            )),
                        );
                    }
                }
            }
        }
        self.reap_cancelled();
        self.reap_terminal_streams(shared);
        for id in self.active.keys().copied().collect::<Vec<_>>() {
            self.backend.cancel(id);
        }
        self.active.clear();
        for id in self.active_streams.keys().copied().collect::<Vec<_>>() {
            self.backend.cancel(id);
        }
        self.active_streams.clear();
        #[cfg(feature = "resolver")]
        for id in self.active_resolves.keys().copied().collect::<Vec<_>>() {
            self.backend.cancel(id);
        }
        #[cfg(feature = "resolver")]
        self.active_resolves.clear();
        self.backend.shutdown()
    }

    pub(crate) fn transport_wait(&self) -> Option<std::time::Duration> {
        let no_active_resolves = {
            #[cfg(feature = "resolver")]
            {
                self.active_resolves.is_empty()
            }
            #[cfg(not(feature = "resolver"))]
            {
                true
            }
        };
        if self.active.is_empty()
            && self.active_streams.is_empty()
            && no_active_resolves
            && !self.backend.wants_poll_without_requests()
        {
            return None;
        }
        match self.backend.poll_mode() {
            PollMode::CommandDriven => None,
            PollMode::Interruptible { max_wait } => Some(max_wait),
        }
    }

    fn reap_cancelled(&mut self) -> bool {
        let terminal = self
            .active
            .iter()
            .filter(|(_id, state)| state.is_terminal())
            .map(|(id, _state)| *id)
            .collect::<Vec<_>>();
        for id in &terminal {
            self.backend.cancel(*id);
            self.active.remove(id);
        }
        #[cfg(feature = "resolver")]
        let resolve_terminal = self
            .active_resolves
            .iter()
            .filter(|(_id, state)| state.is_terminal())
            .map(|(id, _state)| *id)
            .collect::<Vec<_>>();
        #[cfg(feature = "resolver")]
        for id in &resolve_terminal {
            self.backend.cancel(*id);
            self.active_resolves.remove(id);
        }
        let reaped = !terminal.is_empty();
        #[cfg(feature = "resolver")]
        let reaped = reaped || !resolve_terminal.is_empty();
        reaped
    }

    fn commit_backend_completions(
        &mut self,
        shared: &Arc<Shared>,
        completions: Vec<BackendCompletion>,
    ) {
        for BackendCompletion { id, completion } in completions {
            if let Some(state) = self.active.remove(&id) {
                shared.complete_state(&state, completion);
            }
        }
    }

    #[cfg(feature = "resolver")]
    fn commit_resolve_completions(
        &mut self,
        shared: &Arc<Shared>,
        completions: Vec<BackendResolveCompletion>,
    ) {
        for BackendResolveCompletion { id, completion } in completions {
            if let Some(state) = self.active_resolves.remove(&id) {
                shared.complete_resolve_state(&state, completion);
            }
        }
    }

    fn reap_terminal_streams(&mut self, shared: &Arc<Shared>) -> bool {
        let terminal = self
            .active_streams
            .iter()
            .filter(|(_id, state)| state.is_terminal())
            .map(|(id, _state)| *id)
            .collect::<Vec<_>>();
        for id in &terminal {
            self.backend.cancel(*id);
            if let Some(state) = self.active_streams.remove(id) {
                shared.finish_stream_state(&state);
            }
        }
        !terminal.is_empty()
    }
}

#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn spawned_main<B: Backend + ?Sized>(
    shared: Arc<Shared>,
    mut reactor: ReactorCore<B>,
) -> Result<(), ShutdownError> {
    contain_reactor_panic(Arc::clone(&shared), || {
        spawned_main_inner(shared, &mut reactor)
    })
}

fn spawned_main_inner<B: Backend + ?Sized>(
    shared: Arc<Shared>,
    reactor: &mut ReactorCore<B>,
) -> Result<(), ShutdownError> {
    let mut seen_generation = 0;
    loop {
        let deadline = if let Some(max_wait) = reactor.transport_wait() {
            interruptible_poll_deadline(max_wait)
        } else {
            shared
                .queue
                .wait_for_signal(&mut seen_generation, &shared.stopped);
            Instant::now()
        };

        if let Err(error) = reactor.drive(&shared, deadline) {
            shared.fail_all(error.clone());
            let shutdown = reactor.shutdown(&shared);
            shared.mark_stopped();
            return shutdown.and(Err(ShutdownError::new(error)));
        }

        if shared.stopped.load(std::sync::atomic::Ordering::Acquire) {
            let result = reactor.shutdown(&shared);
            if result.is_ok() {
                shared.mark_stopped();
            }
            return result;
        }
    }
}

#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn spawned_main_factory(
    shared: Arc<Shared>,
    factory: Box<dyn BackendFactory>,
) -> Result<(), ShutdownError> {
    let mut backend = None;
    contain_reactor_panic(Arc::clone(&shared), || match factory.create(&shared) {
        Ok(created) => {
            backend = Some(created);
            Ok(())
        }
        Err(error) => {
            shared.fail_all(error.clone());
            shared.mark_stopped();
            Err(ShutdownError::new(error))
        }
    })?;
    let mut reactor =
        ReactorCore::new(backend.expect("successful backend factory did not return its backend"));
    contain_reactor_panic(Arc::clone(&shared), || {
        spawned_main_inner(shared, &mut reactor)
    })
}

fn contain_reactor_panic(
    shared: Arc<Shared>,
    operation: impl FnOnce() -> Result<(), ShutdownError>,
) -> Result<(), ShutdownError> {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(_panic) => {
            shared.queue.set_external_waker(None);
            let shutdown = reactor_panicked();
            shared.fail_all(shutdown.error().clone());
            shared.mark_stopped();
            Err(shutdown)
        }
    }
}

pub(crate) fn reactor_panicked() -> ShutdownError {
    ShutdownError::new(Error::new(
        ErrorKind::Internal,
        "NBReq reactor thread panicked",
    ))
}
