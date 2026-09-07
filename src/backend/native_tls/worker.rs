//! Engine-owned, bounded TLS handshake steps. Workers never own NBReq sockets or HTTP bodies.

use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use super::{TlsProgress, TlsSession};
use crate::backend::native::SlotId;
use crate::backend::native_poll::NativeWaker;
use crate::{Error, ErrorKind, ShutdownError};

const WORKERS: usize = 2;
const QUEUED: usize = 4;
pub(crate) const INPUT_WINDOW: usize = 16 * 1024;

// A ticket outlives every byte belonging to its job, even on cancellation or panic. Completed
// results still occupy tickets until the owner receives or discards them.
struct Ticket(Arc<AtomicUsize>);

impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Job {
    slot: SlotId,
    session: TlsSession,
    input: Vec<u8>,
    deadline: Option<Instant>,
    cancelled: Arc<AtomicBool>,
    ticket: Ticket,
}

pub(crate) struct Finished {
    pub(crate) slot: SlotId,
    pub(crate) session: TlsSession,
    pub(crate) progress: Result<TlsProgress, Error>,
    _ticket: Ticket,
}

#[derive(Default)]
struct State {
    queued: VecDeque<Job>,
    running: HashMap<SlotId, Arc<AtomicBool>>,
    finished: VecDeque<Finished>,
    stopping: bool,
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    occupied: Arc<AtomicUsize>,
    waker: NativeWaker,
}

pub(crate) struct HandshakeWorkers {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl HandshakeWorkers {
    pub(crate) fn new(waker: NativeWaker) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                ready: Condvar::new(),
                occupied: Arc::new(AtomicUsize::new(0)),
                waker,
            }),
            threads: Vec::new(),
        }
    }

    pub(crate) fn has_capacity(&self) -> bool {
        let state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        !state.stopping
            && state.queued.len() < QUEUED
            && self.shared.occupied.load(Ordering::Acquire) < WORKERS + QUEUED
    }

    // Only the network owner submits, so capacity cannot decrease between its capacity check
    // and submit. Workers only move existing jobs or release tickets.
    pub(crate) fn submit(
        &mut self,
        slot: SlotId,
        session: TlsSession,
        input: Vec<u8>,
        deadline: Option<Instant>,
    ) -> Result<(), Error> {
        if input.len() > INPUT_WINDOW {
            return Err(Error::new(
                ErrorKind::Internal,
                "TLS handshake input exceeded its read allowance",
            ));
        }
        if self.threads.is_empty() {
            for index in 0..WORKERS {
                let shared = Arc::clone(&self.shared);
                match thread::Builder::new()
                    .name(format!("nbreq-tls-{index}"))
                    .spawn(move || worker_main(shared))
                {
                    Ok(handle) => self.threads.push(handle),
                    Err(error) => {
                        let _ = self.shutdown();
                        // No job has been admitted during lazy startup. Leave an empty service
                        // so a transient thread-creation failure cannot strand future requests.
                        self.shared
                            .state
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .stopping = false;
                        return Err(Error::new(
                            ErrorKind::Internal,
                            format!("TLS worker creation failed: {error}"),
                        ));
                    }
                }
            }
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.stopping
            || state.queued.len() >= QUEUED
            || self.shared.occupied.load(Ordering::Acquire) >= WORKERS + QUEUED
        {
            return Err(Error::new(
                ErrorKind::Internal,
                "TLS handshake submitted without capacity",
            ));
        }
        self.shared.occupied.fetch_add(1, Ordering::AcqRel);
        state.queued.push_back(Job {
            slot,
            session,
            input,
            deadline,
            cancelled: Arc::new(AtomicBool::new(false)),
            ticket: Ticket(Arc::clone(&self.shared.occupied)),
        });
        self.shared.ready.notify_one();
        Ok(())
    }

    pub(crate) fn take_finished(&self) -> Option<Finished> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .finished
            .pop_front()
    }

    pub(crate) fn cancel(&self, slot: SlotId) {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cancelled) = state.running.get(&slot) {
            cancelled.store(true, Ordering::Release);
        }
        let queued = state
            .queued
            .iter()
            .position(|job| job.slot == slot)
            .and_then(|index| state.queued.remove(index));
        let finished = state
            .finished
            .iter()
            .position(|job| job.slot == slot)
            .and_then(|index| state.finished.remove(index));
        drop(state);
        drop(queued);
        drop(finished);
    }

    pub(crate) fn stop(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        state.stopping = true;
        for cancelled in state.running.values() {
            cancelled.store(true, Ordering::Release);
        }
        let queued = std::mem::take(&mut state.queued);
        let finished = std::mem::take(&mut state.finished);
        drop(state);
        drop(queued);
        drop(finished);
        self.shared.ready.notify_all();
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), ShutdownError> {
        self.stop();
        let mut panicked = false;
        for handle in self.threads.drain(..) {
            panicked |= handle.join().is_err();
        }
        if panicked {
            Err(ShutdownError::new(Error::new(
                ErrorKind::Internal,
                "TLS worker panicked",
            )))
        } else {
            Ok(())
        }
    }
}

impl Drop for HandshakeWorkers {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn worker_main(shared: Arc<Shared>) {
    loop {
        let mut state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut job = loop {
            if state.stopping {
                return;
            }
            if let Some(job) = state.queued.pop_front() {
                state.running.insert(job.slot, Arc::clone(&job.cancelled));
                break job;
            }
            state = shared.ready.wait(state).unwrap_or_else(|p| p.into_inner());
        };
        drop(state);
        let progress = catch_unwind(AssertUnwindSafe(|| {
            if job.cancelled.load(Ordering::Acquire) {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "cancelled TLS handshake step",
                ));
            }
            if job
                .deadline
                .is_some_and(|deadline| deadline <= Instant::now())
            {
                // The owner supplies only immutable total/connect deadlines and classifies
                // the expired request before delivering this result.
                return Err(Error::timeout(
                    crate::TimeoutKind::Unknown,
                    "TLS handshake deadline elapsed in queue",
                ));
            }
            job.session.receive(&job.input)
        }))
        .unwrap_or_else(|_| {
            Err(Error::new(
                ErrorKind::Internal,
                "TLS handshake processing panicked",
            ))
        });
        let Job {
            slot,
            session,
            input,
            cancelled,
            ticket,
            ..
        } = job;
        drop(input);
        let finished = Finished {
            slot,
            session,
            progress,
            _ticket: ticket,
        };
        let mut state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
        state.running.remove(&slot);
        if !state.stopping && !cancelled.load(Ordering::Acquire) {
            state.finished.push_back(finished);
            drop(state);
        } else {
            drop(state);
            drop(finished);
        }
        // A failed wake still gets serviced by the owner's bounded safety poll.
        let _ = shared.waker.wake();
    }
}

#[cfg(test)]
mod tests;
