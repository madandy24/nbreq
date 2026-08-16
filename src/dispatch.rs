//! Bounded owned callback-event dispatch, independent of all Engine/network state.

use std::collections::{HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::callback::{CallbackCompletion, DetachedCallbacks};
use crate::context::{ContextGuard, ContextKind};
use crate::{CallbackDispatch, Error, ErrorKind, RequestId, ShutdownError};

pub(crate) struct CallbackJob {
    request_id: RequestId,
    kind: CallbackKind,
    callback: Box<dyn FnOnce() + Send + 'static>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CallbackKind {
    Progress,
    Terminal,
}

impl CallbackJob {
    pub(crate) fn new(request_id: RequestId, callback: impl FnOnce() + Send + 'static) -> Self {
        Self {
            request_id,
            kind: CallbackKind::Terminal,
            callback: Box::new(callback),
        }
    }

    #[cfg(test)]
    fn progress(request_id: RequestId, callback: impl FnOnce() + Send + 'static) -> Self {
        Self {
            request_id,
            kind: CallbackKind::Progress,
            callback: Box::new(callback),
        }
    }
}

struct DispatchState {
    sealed: bool,
    queue: VecDeque<CallbackJob>,
    active_requests: HashSet<RequestId>,
    running: usize,
    workers_alive: usize,
}

pub(crate) struct CallbackDomain {
    engine_id: u64,
    capacity: usize,
    state: Mutex<DispatchState>,
    changed: Condvar,
    completion: Arc<CallbackCompletion>,
    panic_count: AtomicUsize,
    #[cfg(test)]
    worker_exit_hook: Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
}

impl CallbackDomain {
    fn new(engine_id: u64, capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            engine_id,
            capacity,
            state: Mutex::new(DispatchState {
                sealed: false,
                queue: VecDeque::new(),
                active_requests: HashSet::new(),
                running: 0,
                workers_alive: 0,
            }),
            changed: Condvar::new(),
            completion: CallbackCompletion::pending(),
            panic_count: AtomicUsize::new(0),
            #[cfg(test)]
            worker_exit_hook: Mutex::new(None),
        })
    }

    pub(crate) fn enqueue_terminal(&self, job: CallbackJob) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        if state.sealed {
            return false;
        }
        if state.queue.len() == self.capacity {
            let progress = state
                .queue
                .iter()
                .position(|queued| queued.kind == CallbackKind::Progress)
                .expect("terminal callback capacity invariant was violated");
            state.queue.remove(progress);
        }
        state.queue.push_back(job);
        self.changed.notify_all();
        true
    }

    fn worker_started(&self) {
        lock_unpoisoned(&self.state).workers_alive += 1;
    }

    #[cfg(test)]
    fn enqueue_progress(&self, job: CallbackJob) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        if state.sealed {
            return false;
        }
        if let Some(position) = state.queue.iter().position(|queued| {
            queued.kind == CallbackKind::Progress && queued.request_id == job.request_id
        }) {
            state.queue[position] = job;
            return true;
        }
        if state.queue.len() == self.capacity {
            return false;
        }
        state.queue.push_back(job);
        self.changed.notify_all();
        true
    }

    fn take_runnable(state: &mut DispatchState) -> Option<CallbackJob> {
        let position = state
            .queue
            .iter()
            .position(|job| !state.active_requests.contains(&job.request_id))?;
        let job = state.queue.remove(position)?;
        state.active_requests.insert(job.request_id);
        state.running += 1;
        Some(job)
    }

    fn run_job(&self, job: CallbackJob) {
        let request_id = job.request_id;
        let _context = ContextGuard::enter(self.engine_id, ContextKind::Callback);
        if catch_unwind(AssertUnwindSafe(job.callback)).is_err() {
            self.panic_count.fetch_add(1, Ordering::Relaxed);
        }

        let mut state = lock_unpoisoned(&self.state);
        state.running -= 1;
        state.active_requests.remove(&request_id);
        self.maybe_complete(&state);
        self.changed.notify_all();
    }

    fn worker(self: Arc<Self>) {
        loop {
            let job = {
                let mut state = lock_unpoisoned(&self.state);
                loop {
                    if let Some(job) = Self::take_runnable(&mut state) {
                        break Some(job);
                    }
                    if state.sealed && state.queue.is_empty() {
                        state.workers_alive -= 1;
                        self.maybe_complete(&state);
                        self.changed.notify_all();
                        break None;
                    }
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            };

            match job {
                Some(job) => self.run_job(job),
                None => {
                    #[cfg(test)]
                    self.run_worker_exit_hook();
                    return;
                }
            }
        }
    }

    fn drain_inline(&self) {
        loop {
            let job = {
                let mut state = lock_unpoisoned(&self.state);
                Self::take_runnable(&mut state)
            };
            match job {
                Some(job) => self.run_job(job),
                None => {
                    let state = lock_unpoisoned(&self.state);
                    self.maybe_complete(&state);
                    return;
                }
            }
        }
    }

    fn seal(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.sealed = true;
        self.maybe_complete(&state);
        self.changed.notify_all();
    }

    #[cfg(test)]
    fn is_sealed(&self) -> bool {
        lock_unpoisoned(&self.state).sealed
    }

    fn maybe_complete(&self, state: &DispatchState) {
        if state.sealed && state.queue.is_empty() && state.running == 0 && state.workers_alive == 0
        {
            self.completion.mark_complete();
        }
    }

    #[cfg(test)]
    pub(crate) fn panic_count(&self) -> usize {
        self.panic_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn set_worker_exit_hook(&self, hook: impl FnOnce() + Send + 'static) {
        *lock_unpoisoned(&self.worker_exit_hook) = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_worker_exit_hook(&self) {
        if let Some(hook) = lock_unpoisoned(&self.worker_exit_hook).take() {
            hook();
        }
    }
}

pub(crate) struct DispatcherOwner {
    domain: Arc<CallbackDomain>,
    workers: Vec<JoinHandle<()>>,
    inline: bool,
}

impl DispatcherOwner {
    pub(crate) fn new(
        engine_id: u64,
        capacity: usize,
        dispatch: CallbackDispatch,
    ) -> Result<Self, Error> {
        let worker_count = match dispatch {
            CallbackDispatch::Inline => 0,
            CallbackDispatch::Workers(workers) => workers.get(),
        };
        let domain = CallbackDomain::new(engine_id, capacity);
        let mut workers = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            let worker_domain = Arc::clone(&domain);
            match thread::Builder::new()
                .name(format!("nbreq-callback-{engine_id}-{index}"))
                .spawn(move || worker_domain.worker())
            {
                Ok(worker) => {
                    domain.worker_started();
                    workers.push(worker);
                }
                Err(error) => {
                    domain.seal();
                    let _completion_result = domain.completion.wait();
                    for worker in workers {
                        let _worker_result = worker.join();
                    }
                    return Err(Error::new(
                        ErrorKind::Internal,
                        format!("failed to spawn NBReq callback worker: {error}"),
                    ));
                }
            }
        }
        Ok(Self {
            domain,
            workers,
            inline: worker_count == 0,
        })
    }

    pub(crate) fn domain(&self) -> Arc<CallbackDomain> {
        Arc::clone(&self.domain)
    }

    pub(crate) fn drain_inline(&self) {
        if self.inline {
            self.domain.drain_inline();
        }
    }

    pub(crate) fn seal(&self) {
        self.domain.seal();
    }

    pub(crate) fn finish(mut self) -> Result<(), ShutdownError> {
        self.domain.seal();
        self.drain_inline();
        self.domain.completion.wait()?;
        self.join_workers();
        Ok(())
    }

    pub(crate) fn finish_for(
        mut self,
        duration: Duration,
    ) -> Result<Option<DetachedCallbacks>, ShutdownError> {
        self.domain.seal();
        self.drain_inline();
        if self.domain.completion.wait_for(duration)? {
            self.join_workers();
            Ok(None)
        } else {
            Ok(Some(DetachedCallbacks::new(
                Arc::clone(&self.domain.completion),
                std::mem::take(&mut self.workers),
            )))
        }
    }

    fn join_workers(&mut self) {
        for worker in self.workers.drain(..) {
            let _panic_was_contained = worker.join();
        }
    }

    #[cfg(test)]
    pub(crate) fn is_sealed(&self) -> bool {
        self.domain.is_sealed()
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn id(sequence: u64) -> RequestId {
        RequestId {
            engine: 77,
            sequence,
        }
    }

    #[test]
    fn worker_pool_serializes_each_request_but_runs_peers() {
        let workers = std::num::NonZeroUsize::new(2).expect("two is non-zero");
        let owner = DispatcherOwner::new(77, 4, CallbackDispatch::Workers(workers))
            .expect("dispatcher must construct");
        let domain = owner.domain();
        let (first_started_tx, first_started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let (peer_tx, peer_rx) = mpsc::channel();

        assert!(domain.enqueue_terminal(CallbackJob::new(id(1), move || {
            first_started_tx
                .send(())
                .expect("test receiver must remain");
            release_rx.recv().expect("release must arrive");
        })));
        assert!(domain.enqueue_terminal(CallbackJob::new(id(1), move || {
            second_tx.send(()).expect("test receiver must remain");
        })));
        assert!(domain.enqueue_terminal(CallbackJob::new(id(2), move || {
            peer_tx.send(()).expect("test receiver must remain");
        })));

        first_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first callback must start");
        peer_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("different request may run concurrently");
        assert!(second_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release_tx.send(()).expect("first callback must remain");
        second_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("same-request callback must follow first");
        owner.finish().expect("dispatcher must finish");
    }

    #[test]
    fn callback_panic_is_counted_and_worker_survives() {
        let workers = std::num::NonZeroUsize::new(1).expect("one is non-zero");
        let owner = DispatcherOwner::new(77, 2, CallbackDispatch::Workers(workers))
            .expect("dispatcher must construct");
        let domain = owner.domain();
        let observed = Arc::clone(&domain);
        let (survivor_tx, survivor_rx) = mpsc::channel();

        assert!(domain.enqueue_terminal(CallbackJob::new(id(1), || {
            panic!("deliberate callback panic")
        })));
        assert!(domain.enqueue_terminal(CallbackJob::new(id(2), move || {
            survivor_tx.send(()).expect("test receiver must remain");
        })));
        survivor_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker must survive callback panic");
        owner.finish().expect("dispatcher must finish");
        assert_eq!(observed.panic_count(), 1);
    }

    #[test]
    fn progress_coalesces_and_terminal_events_displace_progress() {
        let owner = DispatcherOwner::new(77, 1, CallbackDispatch::Inline)
            .expect("dispatcher must construct");
        let domain = owner.domain();
        let (progress_tx, progress_rx) = mpsc::channel();

        assert!(domain.enqueue_progress(CallbackJob::progress(id(1), {
            let progress_tx = progress_tx.clone();
            move || progress_tx.send(1).expect("test receiver must remain")
        })));
        assert!(domain.enqueue_progress(CallbackJob::progress(id(1), {
            let progress_tx = progress_tx.clone();
            move || progress_tx.send(2).expect("test receiver must remain")
        })));
        owner.drain_inline();
        assert_eq!(
            progress_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("coalesced progress must run"),
            2
        );
        assert!(progress_rx.try_recv().is_err());

        let (terminal_tx, terminal_rx) = mpsc::channel();
        assert!(
            domain.enqueue_progress(CallbackJob::progress(id(1), move || {
                panic!("displaced progress must not run");
            }))
        );
        assert!(domain.enqueue_terminal(CallbackJob::new(id(2), move || {
            terminal_tx.send(()).expect("test receiver must remain");
        })));
        owner.drain_inline();
        terminal_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("terminal callback must survive pressure");
        owner.finish().expect("dispatcher must finish");
    }

    #[test]
    fn detached_wait_joins_worker_after_domain_completion() {
        let workers = std::num::NonZeroUsize::new(1).expect("one is non-zero");
        let owner = DispatcherOwner::new(77, 1, CallbackDispatch::Workers(workers))
            .expect("dispatcher must construct");
        let domain = owner.domain();
        let (callback_started_tx, callback_started_rx) = mpsc::channel();
        let (release_callback_tx, release_callback_rx) = mpsc::channel();
        let (exit_hook_tx, exit_hook_rx) = mpsc::channel();
        let (release_exit_tx, release_exit_rx) = mpsc::channel();

        domain.set_worker_exit_hook(move || {
            exit_hook_tx.send(()).expect("test receiver must remain");
            release_exit_rx
                .recv()
                .expect("worker exit must be released");
        });
        assert!(domain.enqueue_terminal(CallbackJob::new(id(1), move || {
            callback_started_tx
                .send(())
                .expect("test receiver must remain");
            release_callback_rx
                .recv()
                .expect("callback must be released");
        })));
        callback_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback must start");
        let detached = owner
            .finish_for(Duration::ZERO)
            .expect("timed finish must succeed")
            .expect("running callback must detach");

        release_callback_tx
            .send(())
            .expect("callback must remain alive");
        exit_hook_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker must reach its exit boundary");
        assert!(!detached.is_complete());

        let (waited_tx, waited_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let result = detached.wait();
            waited_tx.send(result).expect("test receiver must remain");
        });
        assert!(waited_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release_exit_tx.send(()).expect("worker must remain alive");
        waited_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("detached wait must return after worker exit")
            .expect("detached worker must join cleanly");
        waiter.join().expect("waiter must join");
    }

    #[test]
    fn terminal_enqueue_after_seal_is_rejected_without_panicking() {
        let owner = DispatcherOwner::new(77, 1, CallbackDispatch::Inline)
            .expect("dispatcher must construct");
        let domain = owner.domain();
        domain.seal();
        assert!(!domain.enqueue_terminal(CallbackJob::new(id(1), || {
            panic!("sealed callback must not run");
        })));
        owner.finish().expect("sealed dispatcher must finish");
    }
}
