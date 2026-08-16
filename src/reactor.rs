use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Instant;

#[cfg(feature = "curl-pilot")]
use crate::backend::BackendFactory;
use crate::backend::{Backend, BackendCompletion, PollMode, interruptible_poll_deadline};
use crate::registry::{RequestState, Shared};
use crate::{DriveStatus, Error, ErrorKind, RequestId, ShutdownError};

pub(crate) struct ReactorCore<B: Backend + ?Sized> {
    backend: Box<B>,
    active: HashMap<RequestId, Arc<RequestState>>,
}

impl<B: Backend + ?Sized> ReactorCore<B> {
    pub(crate) fn new(backend: Box<B>) -> Self {
        Self {
            backend,
            active: HashMap::new(),
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
            let id = submission.state.id();
            if submission.state.is_terminal() {
                continue;
            }
            match self
                .backend
                .submit(id, submission.request, submission.accepted_at)
            {
                Some(completion) => {
                    shared.complete_state(&submission.state, completion);
                }
                None => {
                    self.active.insert(id, submission.state);
                }
            }
        }

        progressed |= self.reap_cancelled();
        let completions = self.backend.poll(deadline)?;
        if let Some(error) = shared.queue.take_external_wake_failure() {
            return Err(error);
        }
        progressed |= !completions.is_empty();
        self.commit_backend_completions(shared, completions);
        progressed |= self.reap_cancelled();

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
            if !submission.state.is_terminal() {
                shared.complete_state(&submission.state, crate::Completion::Cancelled);
            }
        }
        self.reap_cancelled();
        for id in self.active.keys().copied().collect::<Vec<_>>() {
            self.backend.cancel(id);
        }
        self.active.clear();
        self.backend.shutdown()
    }

    pub(crate) fn transport_wait(&self) -> Option<std::time::Duration> {
        if self.active.is_empty() {
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
        !terminal.is_empty()
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
}

#[cfg_attr(feature = "curl-pilot", allow(dead_code))]
pub(crate) fn spawned_main<B: Backend + ?Sized>(
    shared: Arc<Shared>,
    reactor: ReactorCore<B>,
) -> Result<(), ShutdownError> {
    contain_reactor_panic(Arc::clone(&shared), move || {
        spawned_main_inner(shared, reactor)
    })
}

fn spawned_main_inner<B: Backend + ?Sized>(
    shared: Arc<Shared>,
    mut reactor: ReactorCore<B>,
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

#[cfg(feature = "curl-pilot")]
pub(crate) fn spawned_main_factory(
    shared: Arc<Shared>,
    factory: Box<dyn BackendFactory>,
) -> Result<(), ShutdownError> {
    contain_reactor_panic(Arc::clone(&shared), move || match factory.create(&shared) {
        Ok(backend) => spawned_main_inner(shared, ReactorCore::new(backend)),
        Err(error) => {
            shared.fail_all(error.clone());
            shared.mark_stopped();
            Err(ShutdownError::new(error))
        }
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
