//! Isolated live TCP queue, drop, wakeup, and pump-occupancy seam.
//!
//! F2.1 has no sockets. The owner half is the reactor-facing pump; public connection types hold
//! the user half. Moving bytes into the pump does not release send-window capacity. Production
//! `TcpConnector` still does not construct this state, so the owner APIs are test-only until F2.2.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::{
    TcpFinishError, TcpFinishStatus, TcpRead, TcpSendError, TcpSendErrorKind, TcpStreamError,
};
use crate::context;
use crate::dispatch::CallbackJob;
use crate::registry::{AdmissionPermit, Shared};
use crate::{Error, ErrorKind, RequestId, RunMode};

pub(crate) type EngineWaker = Arc<dyn Fn() + Send + Sync + 'static>;
type TcpFinishCallback = Box<dyn FnOnce(Result<(), TcpFinishError>) + Send>;

#[derive(Clone, Debug)]
pub(crate) enum TcpAbort {
    Reset,
    Cancelled,
    Failed(Error),
    EngineStopped,
}

pub(crate) struct TcpIoConfig {
    pub(crate) engine_id: u64,
    pub(crate) request_id: RequestId,
    pub(crate) shared: Arc<Shared>,
    pub(crate) run_mode: RunMode,
    pub(crate) send_window: usize,
    pub(crate) receive_window: usize,
    pub(crate) local: SocketAddr,
    pub(crate) peer: SocketAddr,
    pub(crate) engine_waker: Option<EngineWaker>,
    pub(crate) on_release: Box<dyn FnOnce() + Send>,
}

pub(crate) struct TcpIoShared {
    engine_id: u64,
    request_id: RequestId,
    shared: Arc<Shared>,
    run_mode: RunMode,
    send_window: usize,
    receive_window: usize,
    state: Mutex<TcpIoState>,
    changed: Condvar,
    engine_waker: Mutex<Option<EngineWaker>>,
    on_release: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    released: AtomicBool,
}

struct TcpIoState {
    outbound: VecDeque<Vec<u8>>,
    outbound_bytes: usize,
    pump: VecDeque<Vec<u8>>,
    pump_offset: usize,
    pump_bytes: usize,
    inbound: VecDeque<Vec<u8>>,
    inbound_offset: usize,
    inbound_bytes: usize,
    local: SocketAddr,
    peer: SocketAddr,
    write_finish_requested: bool,
    write_finished: bool,
    peer_fin: bool,
    read_eof_observed: bool,
    abort: Option<TcpAbort>,
    finish_callback: Option<TcpFinishCallback>,
    finish_callback_permit: Option<AdmissionPermit>,
    finish_callback_active: bool,
}

pub(crate) struct TcpIoOwner {
    io: Arc<TcpIoShared>,
}

impl TcpIoShared {
    pub(crate) fn pair(config: TcpIoConfig) -> (Arc<Self>, TcpIoOwner) {
        assert!(
            config.send_window > 0 && config.receive_window > 0,
            "TCP queue windows must be greater than zero"
        );
        let io = Arc::new(Self {
            engine_id: config.engine_id,
            request_id: config.request_id,
            shared: config.shared,
            run_mode: config.run_mode,
            send_window: config.send_window,
            receive_window: config.receive_window,
            state: Mutex::new(TcpIoState {
                outbound: VecDeque::new(),
                outbound_bytes: 0,
                pump: VecDeque::new(),
                pump_offset: 0,
                pump_bytes: 0,
                inbound: VecDeque::new(),
                inbound_offset: 0,
                inbound_bytes: 0,
                local: config.local,
                peer: config.peer,
                write_finish_requested: false,
                write_finished: false,
                peer_fin: false,
                read_eof_observed: false,
                abort: None,
                finish_callback: None,
                finish_callback_permit: None,
                finish_callback_active: false,
            }),
            changed: Condvar::new(),
            engine_waker: Mutex::new(config.engine_waker),
            on_release: Mutex::new(Some(config.on_release)),
            released: AtomicBool::new(false),
        });
        let owner = TcpIoOwner {
            io: Arc::clone(&io),
        };
        (io, owner)
    }

    pub(super) fn local_addr(&self) -> Result<SocketAddr, Error> {
        Ok(lock_unpoisoned(&self.state).local)
    }

    pub(super) fn peer_addr(&self) -> Result<SocketAddr, Error> {
        Ok(lock_unpoisoned(&self.state).peer)
    }

    pub(super) fn try_send(&self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > self.send_window {
            return Err(TcpSendError::new(TcpSendErrorKind::ChunkTooLarge, bytes));
        }

        let mut state = lock_unpoisoned(&self.state);
        if let Some(kind) = send_abort_kind(state.abort.as_ref()) {
            return Err(TcpSendError::new(kind, bytes));
        }
        if state.write_finish_requested || state.write_finished {
            return Err(TcpSendError::new(TcpSendErrorKind::Closed, bytes));
        }
        let consumed = state.outbound_bytes + state.pump_bytes;
        let remaining = self.send_window.saturating_sub(consumed);
        if bytes.len() > remaining {
            return Err(TcpSendError::new(TcpSendErrorKind::WouldBlock, bytes));
        }
        state.outbound_bytes += bytes.len();
        state.outbound.push_back(bytes);
        drop(state);
        self.notify();
        Ok(())
    }

    pub(super) fn send(&self, bytes: Vec<u8>) -> Result<(), TcpSendError> {
        self.forbid_nested_wait();
        if self.run_mode == RunMode::Manual {
            return Err(TcpSendError::new(TcpSendErrorKind::WrongMode, bytes));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let mut remaining = bytes;
        loop {
            if remaining.len() <= self.send_window {
                return self.send_one_blocking(remaining);
            }
            let chunk = remaining[..self.send_window].to_vec();
            match self.send_one_blocking(chunk) {
                Ok(()) => remaining = remaining[self.send_window..].to_vec(),
                Err(error) => {
                    let kind = error.kind();
                    let mut unaccepted = error.into_remaining();
                    unaccepted.extend_from_slice(&remaining[self.send_window..]);
                    return Err(TcpSendError::new(kind, unaccepted));
                }
            }
        }
    }

    fn send_one_blocking(&self, mut bytes: Vec<u8>) -> Result<(), TcpSendError> {
        loop {
            match self.try_send(bytes) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == TcpSendErrorKind::WouldBlock => {
                    bytes = error.into_remaining();
                    let state = lock_unpoisoned(&self.state);
                    let state = self
                        .changed
                        .wait_while(state, |state| {
                            state.abort.is_none()
                                && !state.write_finish_requested
                                && !state.write_finished
                                && state.outbound_bytes + state.pump_bytes == self.send_window
                        })
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    drop(state);
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn try_read(&self, destination: &mut [u8]) -> Result<TcpRead, TcpStreamError> {
        let mut state = lock_unpoisoned(&self.state);
        if let Some(abort) = state.abort.clone() {
            return Err(stream_abort(abort));
        }
        if destination.is_empty() {
            if state.inbound_bytes == 0 && state.peer_fin {
                state.read_eof_observed = true;
                let release = self.maybe_take_release_locked(&mut state);
                drop(state);
                Self::run_release(release);
                self.notify();
                return Ok(TcpRead::Eof);
            }
            if state.inbound_bytes == 0 {
                return Ok(TcpRead::Pending);
            }
            return Ok(TcpRead::Data(0));
        }
        if state.inbound_bytes == 0 {
            if state.peer_fin {
                state.read_eof_observed = true;
                let release = self.maybe_take_release_locked(&mut state);
                drop(state);
                Self::run_release(release);
                self.notify();
                return Ok(TcpRead::Eof);
            }
            return Ok(TcpRead::Pending);
        }

        let mut copied = 0;
        while copied < destination.len() {
            let (offset, chunk_len) = {
                let Some(chunk) = state.inbound.front() else {
                    break;
                };
                (state.inbound_offset, chunk.len())
            };
            let available = chunk_len - offset;
            let take = available.min(destination.len() - copied);
            destination[copied..copied + take].copy_from_slice(
                &state.inbound.front().expect("front exists")[offset..offset + take],
            );
            copied += take;
            state.inbound_offset += take;
            state.inbound_bytes -= take;
            if state.inbound_offset == chunk_len {
                state.inbound.pop_front();
                state.inbound_offset = 0;
            }
        }
        drop(state);
        self.notify();
        Ok(TcpRead::Data(copied))
    }

    pub(super) fn read(&self, destination: &mut [u8]) -> Result<Option<usize>, TcpStreamError> {
        self.require_spawned_read()?;
        loop {
            match self.try_read(destination)? {
                TcpRead::Pending => {
                    let state = lock_unpoisoned(&self.state);
                    let state = self
                        .changed
                        .wait_while(state, |state| {
                            state.abort.is_none() && state.inbound_bytes == 0 && !state.peer_fin
                        })
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    drop(state);
                }
                TcpRead::Data(count) => return Ok(Some(count)),
                TcpRead::Eof => return Ok(None),
            }
        }
    }

    pub(super) fn try_finish(&self) -> Result<TcpFinishStatus, TcpFinishError> {
        let mut state = lock_unpoisoned(&self.state);
        if let Some(abort) = state.abort.clone() {
            return Err(finish_abort(abort));
        }
        if state.write_finished {
            return Ok(TcpFinishStatus::Finished);
        }
        state.write_finish_requested = true;
        drop(state);
        self.notify();
        Ok(TcpFinishStatus::Pending)
    }

    pub(super) fn finish(&self) -> Result<(), TcpFinishError> {
        self.require_spawned_finish()?;
        loop {
            match self.try_finish()? {
                TcpFinishStatus::Finished => return Ok(()),
                TcpFinishStatus::Pending => {
                    let state = lock_unpoisoned(&self.state);
                    let state = self
                        .changed
                        .wait_while(state, |state| {
                            state.abort.is_none() && !state.write_finished
                        })
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    drop(state);
                }
            }
        }
    }

    pub(super) fn finish_with<F>(&self, callback: F) -> Result<(), Error>
    where
        F: FnOnce(Result<(), TcpFinishError>) + Send + 'static,
    {
        {
            let state = lock_unpoisoned(&self.state);
            if state.finish_callback.is_some() {
                drop(callback);
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "a TCP write half already has a finish callback",
                ));
            }
        }

        let (permit, activation) = match self.shared.try_begin_callback_event() {
            Ok(admission) => admission,
            Err(error) => {
                drop(callback);
                return Err(error);
            }
        };
        #[cfg(test)]
        self.shared.fire_callback_activation_hook();

        {
            let mut state = lock_unpoisoned(&self.state);
            if state.finish_callback.is_some() {
                drop(permit);
                drop(callback);
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "a TCP write half already has a finish callback",
                ));
            }
            if state.abort.is_none() && !state.write_finished {
                state.write_finish_requested = true;
            }
            state.finish_callback = Some(Box::new(callback));
            state.finish_callback_permit = Some(permit);
        }
        self.notify();

        let job = {
            let mut state = lock_unpoisoned(&self.state);
            state.finish_callback_active = true;
            self.take_finish_job_locked(&mut state)
        };
        self.dispatch_job(job);
        drop(activation);
        Ok(())
    }

    pub(crate) fn abort(&self, kind: TcpAbort) {
        let mut state = lock_unpoisoned(&self.state);
        if state.abort.is_some() {
            return;
        }
        state.abort = Some(kind);
        let release = self.take_release_locked();
        let job = self.take_finish_job_locked(&mut state);
        drop(state);
        Self::run_release(release);
        self.notify();
        self.dispatch_job(job);
    }

    fn take_finish_job_locked(&self, state: &mut TcpIoState) -> Option<CallbackJob> {
        if !state.finish_callback_active {
            return None;
        }
        let result = if let Some(abort) = state.abort.clone() {
            Err(finish_abort(abort))
        } else if state.write_finished {
            Ok(())
        } else {
            return None;
        };
        let callback = state.finish_callback.take()?;
        let permit = state
            .finish_callback_permit
            .take()
            .expect("a registered finish callback holds its callback-event permit");
        let request_id = self.request_id;
        Some(CallbackJob::new(request_id, move || {
            let _permit = permit;
            callback(result);
        }))
    }

    fn dispatch_job(&self, job: Option<CallbackJob>) {
        if let Some(job) = job {
            self.shared.enqueue_callback_job(job);
        }
    }

    pub(super) fn writer_dropped(&self) {
        let state = lock_unpoisoned(&self.state);
        let requested = state.write_finish_requested || state.write_finished;
        let aborted = state.abort.is_some();
        drop(state);
        if !requested && !aborted {
            self.abort(TcpAbort::Cancelled);
        }
    }

    pub(super) fn reader_dropped(&self) {
        let state = lock_unpoisoned(&self.state);
        let observed = state.read_eof_observed;
        let aborted = state.abort.is_some();
        drop(state);
        if !observed && !aborted {
            self.abort(TcpAbort::Cancelled);
        }
    }

    pub(super) fn session_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    fn require_spawned_read(&self) -> Result<(), TcpStreamError> {
        self.forbid_nested_wait();
        if self.run_mode == RunMode::Manual {
            return Err(TcpStreamError::Operation(Error::new(
                ErrorKind::WrongMode,
                "blocking TCP read requires a spawned Engine",
            )));
        }
        Ok(())
    }

    fn require_spawned_finish(&self) -> Result<(), TcpFinishError> {
        self.forbid_nested_wait();
        if self.run_mode == RunMode::Manual {
            return Err(TcpFinishError::WrongMode);
        }
        Ok(())
    }

    fn forbid_nested_wait(&self) {
        assert!(
            !context::is_active(self.engine_id),
            "blocking wait on the active drive/callback stack is forbidden"
        );
    }

    fn maybe_take_release_locked(
        &self,
        state: &mut TcpIoState,
    ) -> Option<Box<dyn FnOnce() + Send>> {
        if state.abort.is_none() && state.write_finished && state.read_eof_observed {
            self.take_release_locked()
        } else {
            None
        }
    }

    fn take_release_locked(&self) -> Option<Box<dyn FnOnce() + Send>> {
        if self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        lock_unpoisoned(&self.on_release).take()
    }

    fn run_release(release: Option<Box<dyn FnOnce() + Send>>) {
        if let Some(on_release) = release {
            on_release();
        }
    }

    fn notify(&self) {
        self.changed.notify_all();
        if let Some(waker) = lock_unpoisoned(&self.engine_waker).clone() {
            waker();
        }
    }
}

#[cfg_attr(not(feature = "native"), allow(dead_code))]
impl TcpIoOwner {
    pub(crate) fn cancel(&mut self) {
        self.io.abort(TcpAbort::Cancelled);
    }

    pub(crate) fn take_outbound(&mut self) -> Option<Vec<u8>> {
        let mut state = lock_unpoisoned(&self.io.state);
        let chunk = state.outbound.pop_front()?;
        state.outbound_bytes -= chunk.len();
        state.pump_bytes += chunk.len();
        state.pump.push_back(chunk.clone());
        drop(state);
        self.io.notify();
        Some(chunk)
    }

    pub(crate) fn take_outbound_up_to(&mut self, capacity: usize) -> Option<Vec<u8>> {
        if capacity == 0 {
            return None;
        }
        let mut state = lock_unpoisoned(&self.io.state);
        let front = state.outbound.front_mut()?;
        let take = front.len().min(capacity);
        let chunk = if take == front.len() {
            state.outbound.pop_front().expect("front exists")
        } else {
            front.drain(..take).collect()
        };
        state.outbound_bytes -= chunk.len();
        state.pump_bytes += chunk.len();
        state.pump.push_back(chunk.clone());
        drop(state);
        self.io.notify();
        Some(chunk)
    }

    pub(crate) fn write_progress(&mut self, mut nbytes: usize) {
        let mut state = lock_unpoisoned(&self.io.state);
        while nbytes > 0 {
            let (offset, chunk_len) = {
                let Some(front) = state.pump.front() else {
                    break;
                };
                (state.pump_offset, front.len())
            };
            let available = chunk_len - offset;
            let take = available.min(nbytes);
            state.pump_offset += take;
            state.pump_bytes -= take;
            nbytes -= take;
            if state.pump_offset == chunk_len {
                state.pump.pop_front();
                state.pump_offset = 0;
            }
        }
        drop(state);
        self.io.notify();
    }

    pub(crate) fn complete_write_shutdown(&mut self) -> Result<(), Error> {
        let mut state = lock_unpoisoned(&self.io.state);
        if let Some(error) = abort_error(state.abort.as_ref()) {
            return Err(error);
        }
        if !state.write_finish_requested {
            return Err(Error::new(
                ErrorKind::Internal,
                "write shutdown requires an accepted finish request",
            ));
        }
        if state.outbound_bytes != 0 || state.pump_bytes != 0 {
            return Err(Error::new(
                ErrorKind::Internal,
                "write shutdown requires drained accepted output",
            ));
        }
        state.write_finished = true;
        let release = self.io.maybe_take_release_locked(&mut state);
        let job = self.io.take_finish_job_locked(&mut state);
        drop(state);
        TcpIoShared::run_release(release);
        self.io.notify();
        self.io.dispatch_job(job);
        Ok(())
    }

    pub(crate) fn push_inbound(&mut self, bytes: Vec<u8>) -> Result<(), Error> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut state = lock_unpoisoned(&self.io.state);
        if let Some(error) = abort_error(state.abort.as_ref()) {
            return Err(error);
        }
        if state.peer_fin {
            return Err(Error::new(
                ErrorKind::Internal,
                "inbound data after peer FIN",
            ));
        }
        let remaining = self.io.receive_window.saturating_sub(state.inbound_bytes);
        if bytes.len() > remaining {
            return Err(Error::new(
                ErrorKind::Internal,
                "inbound push exceeds the unread receive window",
            ));
        }
        state.inbound_bytes += bytes.len();
        state.inbound.push_back(bytes);
        drop(state);
        self.io.notify();
        Ok(())
    }

    pub(crate) fn peer_closed(&mut self) {
        let mut state = lock_unpoisoned(&self.io.state);
        if state.abort.is_some() {
            return;
        }
        state.peer_fin = true;
        drop(state);
        self.io.notify();
    }

    pub(crate) fn reset(&mut self) {
        self.io.abort(TcpAbort::Reset);
    }

    pub(crate) fn fail(&mut self, error: Error) {
        self.io.abort(TcpAbort::Failed(error));
    }

    pub(crate) fn finish_requested(&self) -> bool {
        lock_unpoisoned(&self.io.state).write_finish_requested
    }

    pub(crate) fn write_finished(&self) -> bool {
        lock_unpoisoned(&self.io.state).write_finished
    }

    pub(crate) fn pump_bytes(&self) -> usize {
        lock_unpoisoned(&self.io.state).pump_bytes
    }

    pub(crate) fn outbound_bytes(&self) -> usize {
        lock_unpoisoned(&self.io.state).outbound_bytes
    }

    pub(crate) fn send_occupancy(&self) -> usize {
        let state = lock_unpoisoned(&self.io.state);
        state.outbound_bytes + state.pump_bytes
    }

    pub(crate) fn read_allowance(&self) -> usize {
        let state = lock_unpoisoned(&self.io.state);
        self.io.receive_window.saturating_sub(state.inbound_bytes)
    }

    pub(crate) fn session_released(&self) -> bool {
        self.io.session_released()
    }
}

impl Drop for TcpIoOwner {
    fn drop(&mut self) {
        let state = lock_unpoisoned(&self.io.state);
        let live = state.abort.is_none() && !(state.write_finished && state.read_eof_observed);
        drop(state);
        if live {
            self.io.abort(TcpAbort::EngineStopped);
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn send_abort_kind(abort: Option<&TcpAbort>) -> Option<TcpSendErrorKind> {
    match abort {
        None => None,
        Some(TcpAbort::Reset) => Some(TcpSendErrorKind::Reset),
        Some(TcpAbort::Cancelled) => Some(TcpSendErrorKind::Cancelled),
        Some(TcpAbort::EngineStopped) => Some(TcpSendErrorKind::EngineStopped),
        Some(TcpAbort::Failed(_)) => Some(TcpSendErrorKind::Closed),
    }
}

fn stream_abort(abort: TcpAbort) -> TcpStreamError {
    match abort {
        TcpAbort::Reset => TcpStreamError::Reset,
        TcpAbort::Cancelled => TcpStreamError::Cancelled,
        TcpAbort::EngineStopped => TcpStreamError::Failed(Error::new(
            ErrorKind::EngineStopped,
            "the owning Engine has stopped",
        )),
        TcpAbort::Failed(error) => TcpStreamError::Failed(error),
    }
}

fn finish_abort(abort: TcpAbort) -> TcpFinishError {
    match abort {
        TcpAbort::Reset => TcpFinishError::Reset,
        TcpAbort::Cancelled => TcpFinishError::Cancelled,
        TcpAbort::EngineStopped => TcpFinishError::EngineStopped,
        TcpAbort::Failed(error) => TcpFinishError::Failed(error),
    }
}

fn abort_error(abort: Option<&TcpAbort>) -> Option<Error> {
    match abort {
        None => None,
        Some(TcpAbort::EngineStopped) => Some(Error::new(
            ErrorKind::EngineStopped,
            "the owning Engine has stopped",
        )),
        Some(TcpAbort::Failed(error)) => Some(error.clone()),
        Some(TcpAbort::Reset) => Some(Error::new(ErrorKind::Transport, "TCP connection was reset")),
        Some(TcpAbort::Cancelled) => Some(Error::new(
            ErrorKind::Internal,
            "TCP connection was cancelled",
        )),
    }
}
