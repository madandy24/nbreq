use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::{Error, ErrorKind, ShutdownError};

/// Result of a consuming timed Engine shutdown.
#[derive(Debug)]
#[non_exhaustive]
pub enum ShutdownOutcome {
    /// Network services and callback dispatch both completed.
    Complete,
    /// Network services stopped, but sealed callback work is still draining.
    CallbacksRemaining(DetachedCallbacks),
}

/// Observable ownership of a sealed callback domain after timed shutdown.
///
/// This value contains no Engine, network, resolver, TLS, or backend state. Dropping it does not
/// interrupt callbacks; the callback domain remains self-owned until complete.
#[derive(Clone, Debug)]
pub struct DetachedCallbacks {
    state: Arc<DetachedState>,
}

#[derive(Debug)]
struct DetachedState {
    complete: Mutex<bool>,
    changed: Condvar,
}

impl DetachedCallbacks {
    /// Returns whether all callbacks have returned and workers have joined.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        match self.state.complete.lock() {
            Ok(complete) => *complete,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// Waits without a library-imposed timeout for the sealed domain to complete.
    pub fn wait(&self) -> Result<(), ShutdownError> {
        let complete = self.lock_complete()?;
        let result = self
            .state
            .changed
            .wait_while(complete, |complete| !*complete)
            .map_err(|_| poisoned_callback_domain())?;
        drop(result);
        Ok(())
    }

    /// Waits up to `duration`, returning `true` only when the domain completed.
    pub fn wait_for(&self, duration: Duration) -> Result<bool, ShutdownError> {
        let complete = self.lock_complete()?;
        if *complete {
            return Ok(true);
        }

        let deadline = Instant::now().checked_add(duration);
        let mut complete = complete;
        loop {
            let remaining = match deadline {
                Some(deadline) => deadline.saturating_duration_since(Instant::now()),
                None => duration,
            };
            if remaining.is_zero() {
                return Ok(*complete);
            }

            let (next, timed_out) = self
                .state
                .changed
                .wait_timeout(complete, remaining)
                .map_err(|_| poisoned_callback_domain())?;
            complete = next;
            if *complete || timed_out.timed_out() {
                return Ok(*complete);
            }
        }
    }

    fn lock_complete(&self) -> Result<std::sync::MutexGuard<'_, bool>, ShutdownError> {
        self.state
            .complete
            .lock()
            .map_err(|_| poisoned_callback_domain())
    }
}

fn poisoned_callback_domain() -> ShutdownError {
    ShutdownError::new(Error::new(
        ErrorKind::Internal,
        "detached callback state was poisoned",
    ))
}
