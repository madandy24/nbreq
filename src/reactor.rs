use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

#[cfg(all(feature = "curl", any(test, feature = "test-support")))]
use crate::backend::BackendFactory;
use crate::backend::{Backend, BackendCompletion, PollMode, long_poll_deadline};
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
        let submissions = shared.queue.drain();
        let mut progressed = !submissions.is_empty();
        for submission in submissions {
            let id = submission.state.id();
            if submission.state.is_terminal() {
                continue;
            }
            match self.backend.submit(id, submission.request) {
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

    fn can_wait_for_transport(&self) -> bool {
        !self.active.is_empty() && self.backend.poll_mode() == PollMode::Interruptible
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

pub(crate) fn spawned_main<B: Backend + ?Sized>(
    shared: Arc<Shared>,
    mut reactor: ReactorCore<B>,
) -> Result<(), ShutdownError> {
    let mut seen_generation = 0;
    loop {
        let deadline = if reactor.can_wait_for_transport() {
            long_poll_deadline()
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

#[cfg(all(feature = "curl", any(test, feature = "test-support")))]
pub(crate) fn spawned_main_factory(
    shared: Arc<Shared>,
    factory: Box<dyn BackendFactory>,
) -> Result<(), ShutdownError> {
    match factory.create(&shared) {
        Ok(backend) => spawned_main(shared, ReactorCore::new(backend)),
        Err(error) => {
            shared.fail_all(error.clone());
            shared.mark_stopped();
            Err(ShutdownError::new(error))
        }
    }
}

pub(crate) fn reactor_panicked() -> ShutdownError {
    ShutdownError::new(Error::new(
        ErrorKind::Internal,
        "NBReq reactor thread panicked",
    ))
}
