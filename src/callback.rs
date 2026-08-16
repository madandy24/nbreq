use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
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
/// The public observation handle is unique and deliberately not `Clone`, keeping DLL-unload and
/// final-wait responsibility obvious. The sealed callback domain itself remains self-owned.
///
/// ```compile_fail
/// use nbreq::DetachedCallbacks;
/// fn require_clone<T: Clone>() {}
/// require_clone::<DetachedCallbacks>();
/// ```
///
/// This value contains no Engine, network, resolver, TLS, or backend state. Dropping it does not
/// interrupt callbacks; the callback domain remains self-owned until complete.
#[derive(Debug)]
pub struct DetachedCallbacks {
    state: Arc<CallbackCompletion>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(Debug)]
pub(crate) struct CallbackCompletion {
    complete: Mutex<bool>,
    changed: Condvar,
}

impl CallbackCompletion {
    pub(crate) fn pending() -> Arc<Self> {
        Arc::new(Self {
            complete: Mutex::new(false),
            changed: Condvar::new(),
        })
    }

    pub(crate) fn mark_complete(&self) {
        *lock_unpoisoned(&self.complete) = true;
        self.changed.notify_all();
    }

    pub(crate) fn is_complete(&self) -> bool {
        *lock_unpoisoned(&self.complete)
    }

    pub(crate) fn wait(&self) -> Result<(), ShutdownError> {
        let complete = lock_unpoisoned(&self.complete);
        let result = self
            .changed
            .wait_while(complete, |complete| !*complete)
            .map_err(|_| poisoned_callback_domain())?;
        drop(result);
        Ok(())
    }

    pub(crate) fn wait_for(&self, duration: Duration) -> Result<bool, ShutdownError> {
        let complete = lock_unpoisoned(&self.complete);
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
                .changed
                .wait_timeout(complete, remaining)
                .map_err(|_| poisoned_callback_domain())?;
            complete = next;
            if *complete || timed_out.timed_out() {
                return Ok(*complete);
            }
        }
    }
}

impl DetachedCallbacks {
    pub(crate) fn new(state: Arc<CallbackCompletion>, workers: Vec<JoinHandle<()>>) -> Self {
        Self {
            state,
            workers: Mutex::new(workers),
        }
    }

    /// Returns whether all callbacks have returned and workers have exited.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state.is_complete() && self.workers_finished()
    }

    /// Waits without a library-imposed timeout for the sealed domain to complete.
    pub fn wait(&self) -> Result<(), ShutdownError> {
        self.state.wait()?;
        self.join_workers()
    }

    /// Waits up to `duration`, returning `true` only after callback workers have exited and joined.
    pub fn wait_for(&self, duration: Duration) -> Result<bool, ShutdownError> {
        let deadline = Instant::now().checked_add(duration);
        if !self.state.wait_for(duration)? {
            return Ok(false);
        }

        while !self.workers_finished() {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(false);
            }
            thread::yield_now();
        }
        self.join_workers()?;
        Ok(true)
    }

    fn workers_finished(&self) -> bool {
        lock_unpoisoned(&self.workers)
            .iter()
            .all(JoinHandle::is_finished)
    }

    fn join_workers(&self) -> Result<(), ShutdownError> {
        let mut worker_panicked = false;
        for worker in lock_unpoisoned(&self.workers).drain(..) {
            worker_panicked |= worker.join().is_err();
        }
        if worker_panicked {
            Err(callback_worker_panicked())
        } else {
            Ok(())
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn poisoned_callback_domain() -> ShutdownError {
    ShutdownError::new(Error::new(
        ErrorKind::Internal,
        "detached callback state was poisoned",
    ))
}

fn callback_worker_panicked() -> ShutdownError {
    ShutdownError::new(Error::new(
        ErrorKind::Internal,
        "detached callback worker panicked",
    ))
}
