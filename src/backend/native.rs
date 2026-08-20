#![cfg_attr(not(test), allow(dead_code))]

//! Private, HTTP-independent native readiness reactor.
//!
//! This module deliberately stops below DNS, TLS, and HTTP. It owns nonblocking sockets, bounded
//! byte queues, readiness registration, deadlines, cancellation, and teardown. Later native
//! protocol layers consume its events without moving sockets or callbacks off the reactor owner.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mio::net::TcpStream;
use mio::{Events, Interest, Poll, Token, Waker};

const WAKE_TOKEN: Token = Token(0);
const FIRST_SOCKET_TOKEN: usize = 1;
const READ_CHUNK: usize = 16 * 1024;
pub(super) const NATIVE_SAFETY_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SlotId {
    index: u32,
    generation: u32,
}

#[derive(Clone)]
pub(crate) struct NativeWaker {
    inner: Arc<Waker>,
}

impl NativeWaker {
    pub(crate) fn wake(&self) -> io::Result<()> {
        self.inner.wake()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeFailureKind {
    Connect,
    Read,
    Write,
    OutboundQueueFull,
    ReceiveLimit,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeFailure {
    pub(crate) kind: NativeFailureKind,
    pub(crate) message: String,
}

impl NativeFailure {
    fn io(kind: NativeFailureKind, operation: &str, error: &io::Error) -> Self {
        Self {
            kind,
            message: format!("native {operation} failed: {error}"),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: NativeFailureKind::Internal,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NativeEvent {
    Connected(SlotId),
    WriteProgress(SlotId),
    WriteDrained(SlotId),
    Data(SlotId, Vec<u8>),
    PeerClosed(SlotId),
    Failed(SlotId, NativeFailure),
    DeadlineExpired(SlotId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionState {
    Connecting,
    Connected,
}

struct Connection {
    token: Token,
    stream: TcpStream,
    registered: bool,
    state: ConnectionState,
    peer_read_closed: bool,
    outbound: VecDeque<u8>,
    outbound_limit: usize,
    received: usize,
    receive_limit: usize,
    deadline: Option<Instant>,
}

struct Slot {
    generation: u32,
    connection: Option<Connection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeadlineEntry {
    when: Instant,
    id: SlotId,
}

impl Ord for DeadlineEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.when
            .cmp(&other.when)
            .then_with(|| self.id.index.cmp(&other.id.index))
            .then_with(|| self.id.generation.cmp(&other.id.generation))
    }
}

impl PartialOrd for DeadlineEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) struct NativeReactor {
    poll: Poll,
    events: Events,
    waker: NativeWaker,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    tokens: HashMap<Token, SlotId>,
    next_token: usize,
    deadlines: BinaryHeap<Reverse<DeadlineEntry>>,
}

impl NativeReactor {
    pub(crate) fn new(event_capacity: usize) -> Result<Self, NativeFailure> {
        let poll = Poll::new().map_err(|error| {
            NativeFailure::io(NativeFailureKind::Internal, "poll creation", &error)
        })?;
        let waker = Waker::new(poll.registry(), WAKE_TOKEN).map_err(|error| {
            NativeFailure::io(NativeFailureKind::Internal, "waker creation", &error)
        })?;
        Ok(Self {
            poll,
            events: Events::with_capacity(event_capacity.max(1)),
            waker: NativeWaker {
                inner: Arc::new(waker),
            },
            slots: Vec::new(),
            free_slots: Vec::new(),
            tokens: HashMap::new(),
            next_token: FIRST_SOCKET_TOKEN,
            deadlines: BinaryHeap::new(),
        })
    }

    pub(crate) fn waker(&self) -> NativeWaker {
        self.waker.clone()
    }

    pub(crate) fn connect(
        &mut self,
        address: SocketAddr,
        deadline: Option<Instant>,
        outbound_limit: usize,
        receive_limit: usize,
    ) -> Result<SlotId, NativeFailure> {
        let (id, slot_index) = self.allocate_slot()?;
        let token = match self.allocate_token() {
            Ok(token) => token,
            Err(error) => {
                self.release_empty_slot(slot_index);
                return Err(error);
            }
        };
        let mut stream = match TcpStream::connect(address) {
            Ok(stream) => stream,
            Err(error) => {
                self.release_empty_slot(slot_index);
                return Err(NativeFailure::io(
                    NativeFailureKind::Connect,
                    "connect start",
                    &error,
                ));
            }
        };
        if let Err(error) = self.poll.registry().register(
            &mut stream,
            token,
            Interest::READABLE.add(Interest::WRITABLE),
        ) {
            self.release_empty_slot(slot_index);
            return Err(NativeFailure::io(
                NativeFailureKind::Internal,
                "socket registration",
                &error,
            ));
        }
        match stream.take_error() {
            Ok(Some(error)) => {
                let _deregister_result = self.poll.registry().deregister(&mut stream);
                self.release_empty_slot(slot_index);
                return Err(NativeFailure::io(
                    NativeFailureKind::Connect,
                    "connect",
                    &error,
                ));
            }
            Err(error) => {
                let _deregister_result = self.poll.registry().deregister(&mut stream);
                self.release_empty_slot(slot_index);
                return Err(NativeFailure::io(
                    NativeFailureKind::Internal,
                    "initial connect status",
                    &error,
                ));
            }
            Ok(None) => {}
        }
        self.tokens.insert(token, id);
        self.slots[slot_index].connection = Some(Connection {
            token,
            stream,
            registered: true,
            state: ConnectionState::Connecting,
            peer_read_closed: false,
            outbound: VecDeque::new(),
            outbound_limit,
            received: 0,
            receive_limit,
            deadline,
        });
        if let Some(when) = deadline {
            self.deadlines.push(Reverse(DeadlineEntry { when, id }));
        }
        Ok(id)
    }

    pub(crate) fn queue_write(&mut self, id: SlotId, bytes: &[u8]) -> Result<(), NativeFailure> {
        let connection = self.connection_mut(id).ok_or_else(|| {
            NativeFailure::internal("native write targeted a stale or closed slot")
        })?;
        if bytes.len()
            > connection
                .outbound_limit
                .saturating_sub(connection.outbound.len())
        {
            return Err(NativeFailure {
                kind: NativeFailureKind::OutboundQueueFull,
                message: "native outbound queue limit exceeded".to_owned(),
            });
        }
        connection.outbound.extend(bytes.iter().copied());
        self.reregister(id)
    }

    pub(crate) fn cancel(&mut self, id: SlotId) -> bool {
        self.remove(id).is_some()
    }

    pub(crate) fn set_deadline(
        &mut self,
        id: SlotId,
        deadline: Option<Instant>,
    ) -> Result<(), NativeFailure> {
        let connection = self.connection_mut(id).ok_or_else(|| {
            NativeFailure::internal("native deadline update targeted a stale or closed slot")
        })?;
        connection.deadline = deadline;
        if let Some(when) = deadline {
            self.deadlines.push(Reverse(DeadlineEntry { when, id }));
        }
        Ok(())
    }

    pub(crate) fn poll(&mut self, deadline: Instant) -> Result<Vec<NativeEvent>, NativeFailure> {
        let wait_until = self
            .nearest_deadline()
            .map_or(deadline, |slot_deadline| slot_deadline.min(deadline));
        let timeout = wait_until.saturating_duration_since(Instant::now());
        self.poll
            .poll(&mut self.events, Some(timeout))
            .map_err(|error| NativeFailure::io(NativeFailureKind::Internal, "poll", &error))?;

        let ready = self
            .events
            .iter()
            .filter(|event| event.token() != WAKE_TOKEN)
            .filter_map(|event| {
                self.tokens.get(&event.token()).copied().map(|id| {
                    (
                        id,
                        event.is_readable() || event.is_read_closed(),
                        event.is_writable() || event.is_write_closed(),
                        event.is_error(),
                    )
                })
            })
            .collect::<Vec<_>>();

        let mut output = Vec::new();
        for (id, readable, writable, error) in ready {
            if !self.contains(id) {
                continue;
            }
            if error {
                if let Some(failure) = self.socket_error(id) {
                    self.remove(id);
                    output.push(NativeEvent::Failed(id, failure));
                    continue;
                }
            }
            let connecting = self
                .connection_mut(id)
                .is_some_and(|connection| connection.state == ConnectionState::Connecting);
            if connecting && !self.finish_connect(id, &mut output) {
                continue;
            }
            if writable && !self.flush_write(id, &mut output) {
                continue;
            }
            if readable {
                self.read_ready(id, &mut output);
            }
            if self.contains(id) {
                self.reregister(id)?;
            }
        }
        self.expire_deadlines(&mut output);
        Ok(output)
    }

    pub(crate) fn active_count(&self) -> usize {
        self.tokens.len()
    }

    pub(crate) fn shutdown(&mut self) {
        let ids = self.tokens.values().copied().collect::<Vec<_>>();
        for id in ids {
            self.remove(id);
        }
    }

    fn allocate_slot(&mut self) -> Result<(SlotId, usize), NativeFailure> {
        if let Some(index) = self.free_slots.pop() {
            let slot = &mut self.slots[index as usize];
            slot.generation = slot.generation.checked_add(1).ok_or_else(|| {
                NativeFailure::internal("native slot generation space is exhausted")
            })?;
            let id = SlotId {
                index,
                generation: slot.generation,
            };
            Ok((id, index as usize))
        } else {
            let index = u32::try_from(self.slots.len())
                .map_err(|_| NativeFailure::internal("native slot index space is exhausted"))?;
            self.slots.push(Slot {
                generation: 1,
                connection: None,
            });
            Ok((
                SlotId {
                    index,
                    generation: 1,
                },
                index as usize,
            ))
        }
    }

    fn allocate_token(&mut self) -> Result<Token, NativeFailure> {
        let token = self.next_token;
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or_else(|| NativeFailure::internal("native poll token space is exhausted"))?;
        Ok(Token(token))
    }

    fn release_empty_slot(&mut self, index: usize) {
        debug_assert!(self.slots[index].connection.is_none());
        self.free_slots.push(index as u32);
    }

    fn contains(&self, id: SlotId) -> bool {
        self.slots
            .get(id.index as usize)
            .is_some_and(|slot| slot.generation == id.generation && slot.connection.is_some())
    }

    fn connection_mut(&mut self, id: SlotId) -> Option<&mut Connection> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.connection.as_mut()
    }

    fn finish_connect(&mut self, id: SlotId, output: &mut Vec<NativeEvent>) -> bool {
        let Some(connection) = self.connection_mut(id) else {
            return false;
        };
        if connection.state == ConnectionState::Connected {
            return true;
        }
        match connection.stream.take_error() {
            Ok(Some(error)) => {
                let failure = NativeFailure::io(NativeFailureKind::Connect, "connect", &error);
                self.remove(id);
                output.push(NativeEvent::Failed(id, failure));
                false
            }
            Err(error) => {
                let failure =
                    NativeFailure::io(NativeFailureKind::Connect, "connect status", &error);
                self.remove(id);
                output.push(NativeEvent::Failed(id, failure));
                false
            }
            Ok(None) => {
                connection.state = ConnectionState::Connected;
                output.push(NativeEvent::Connected(id));
                true
            }
        }
    }

    fn flush_write(&mut self, id: SlotId, output: &mut Vec<NativeEvent>) -> bool {
        let Some(connection) = self.connection_mut(id) else {
            return false;
        };
        if connection.state != ConnectionState::Connected || connection.outbound.is_empty() {
            return true;
        }
        let had_data = !connection.outbound.is_empty();
        let mut wrote_data = false;
        loop {
            let (front, _) = connection.outbound.as_slices();
            if front.is_empty() {
                break;
            }
            match connection.stream.write(front) {
                Ok(0) => {
                    let failure = NativeFailure {
                        kind: NativeFailureKind::Write,
                        message: "native socket write made no progress".to_owned(),
                    };
                    self.remove(id);
                    output.push(NativeEvent::Failed(id, failure));
                    return false;
                }
                Ok(written) => {
                    wrote_data = true;
                    connection.outbound.drain(..written);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    let failure = NativeFailure::io(NativeFailureKind::Write, "write", &error);
                    self.remove(id);
                    output.push(NativeEvent::Failed(id, failure));
                    return false;
                }
            }
        }
        if had_data && connection.outbound.is_empty() {
            output.push(NativeEvent::WriteDrained(id));
        } else if wrote_data {
            output.push(NativeEvent::WriteProgress(id));
        }
        true
    }

    fn read_ready(&mut self, id: SlotId, output: &mut Vec<NativeEvent>) {
        let mut buffer = vec![0_u8; READ_CHUNK];
        loop {
            let read_result = match self.connection_mut(id) {
                Some(connection) if connection.state == ConnectionState::Connected => {
                    connection.stream.read(&mut buffer)
                }
                _ => return,
            };
            match read_result {
                Ok(0) => {
                    if let Some(connection) = self.connection_mut(id) {
                        connection.peer_read_closed = true;
                    }
                    output.push(NativeEvent::PeerClosed(id));
                    return;
                }
                Ok(read) => {
                    let over_limit = self.connection_mut(id).is_none_or(|connection| {
                        connection.received = connection.received.saturating_add(read);
                        connection.received > connection.receive_limit
                    });
                    if over_limit {
                        self.remove(id);
                        output.push(NativeEvent::Failed(
                            id,
                            NativeFailure {
                                kind: NativeFailureKind::ReceiveLimit,
                                message: "native receive limit exceeded".to_owned(),
                            },
                        ));
                        return;
                    }
                    output.push(NativeEvent::Data(id, buffer[..read].to_vec()));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    self.remove(id);
                    output.push(NativeEvent::Failed(
                        id,
                        NativeFailure::io(NativeFailureKind::Read, "read", &error),
                    ));
                    return;
                }
            }
        }
    }

    fn socket_error(&mut self, id: SlotId) -> Option<NativeFailure> {
        let connection = self.connection_mut(id)?;
        match connection.stream.take_error() {
            Ok(Some(error)) => Some(NativeFailure::io(
                if connection.state == ConnectionState::Connecting {
                    NativeFailureKind::Connect
                } else if !connection.outbound.is_empty() {
                    NativeFailureKind::Write
                } else {
                    NativeFailureKind::Read
                },
                "socket readiness",
                &error,
            )),
            Ok(None) => None,
            Err(error) => Some(NativeFailure::io(
                NativeFailureKind::Internal,
                "socket error query",
                &error,
            )),
        }
    }

    fn reregister(&mut self, id: SlotId) -> Result<(), NativeFailure> {
        let poll = &self.poll;
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .ok_or_else(|| {
                NativeFailure::internal("native readiness update targeted a stale slot")
            })?;
        let connection = slot.connection.as_mut().ok_or_else(|| {
            NativeFailure::internal("native readiness update targeted a closed slot")
        })?;
        let interest = if connection.state == ConnectionState::Connecting {
            Some(Interest::READABLE.add(Interest::WRITABLE))
        } else {
            match (connection.peer_read_closed, connection.outbound.is_empty()) {
                (false, false) => Some(Interest::READABLE.add(Interest::WRITABLE)),
                (false, true) => Some(Interest::READABLE),
                (true, false) => Some(Interest::WRITABLE),
                (true, true) => None,
            }
        };
        match (connection.registered, interest) {
            (true, Some(interest)) => poll
                .registry()
                .reregister(&mut connection.stream, connection.token, interest)
                .map_err(|error| {
                    NativeFailure::io(NativeFailureKind::Internal, "socket reregistration", &error)
                }),
            (false, Some(interest)) => {
                poll.registry()
                    .register(&mut connection.stream, connection.token, interest)
                    .map_err(|error| {
                        NativeFailure::io(
                            NativeFailureKind::Internal,
                            "socket registration",
                            &error,
                        )
                    })?;
                connection.registered = true;
                Ok(())
            }
            (true, None) => {
                poll.registry()
                    .deregister(&mut connection.stream)
                    .map_err(|error| {
                        NativeFailure::io(
                            NativeFailureKind::Internal,
                            "socket deregistration",
                            &error,
                        )
                    })?;
                connection.registered = false;
                Ok(())
            }
            (false, None) => Ok(()),
        }
    }

    fn remove(&mut self, id: SlotId) -> Option<()> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        let mut connection = slot.connection.take()?;
        if connection.registered {
            let _deregister_result = self.poll.registry().deregister(&mut connection.stream);
        }
        self.tokens.remove(&connection.token);
        self.free_slots.push(id.index);
        Some(())
    }

    fn nearest_deadline(&mut self) -> Option<Instant> {
        loop {
            let entry = self.deadlines.peek()?.0;
            let current = self
                .connection_mut(entry.id)
                .and_then(|connection| connection.deadline);
            if current == Some(entry.when) {
                return Some(entry.when);
            }
            self.deadlines.pop();
        }
    }

    fn expire_deadlines(&mut self, output: &mut Vec<NativeEvent>) {
        let now = Instant::now();
        while self
            .nearest_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            let Some(Reverse(entry)) = self.deadlines.pop() else {
                break;
            };
            let current = self
                .connection_mut(entry.id)
                .and_then(|connection| connection.deadline);
            if current == Some(entry.when) {
                if let Some(connection) = self.connection_mut(entry.id) {
                    connection.deadline = None;
                }
                output.push(NativeEvent::DeadlineExpired(entry.id));
            }
        }
    }
}

impl Drop for NativeReactor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{IpAddr, Shutdown, TcpListener};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::backend::{Backend, BackendCompletion, BackendFactory, PollMode};
    use crate::registry::Shared;
    use crate::types::http_origin;
    use crate::{
        Completion, Engine, EngineConfig, Error, ErrorKind, Request, RequestId, Response,
        ShutdownError, TimeoutKind, TransportStage, WaitOutcome,
    };

    const TEST_LIMIT: usize = 1024 * 1024;

    struct RawFactory;

    impl BackendFactory for RawFactory {
        fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error> {
            let backend = RawBackend::new()?;
            let waker = backend.reactor.waker();
            shared.queue.set_external_waker(Some(Arc::new(move || {
                waker.wake().map_err(|error| {
                    Error::new(
                        ErrorKind::Internal,
                        format!("native command wake failed: {error}"),
                    )
                })
            })));
            Ok(Box::new(backend))
        }
    }

    struct FailedWakeFactory {
        armed: mpsc::Sender<()>,
        rescue: Arc<(Mutex<bool>, Condvar)>,
    }

    impl BackendFactory for FailedWakeFactory {
        fn create(self: Box<Self>, shared: &Arc<Shared>) -> Result<Box<dyn Backend>, Error> {
            shared.queue.set_external_waker(Some(Arc::new(|| Ok(()))));
            Ok(Box::new(FailedWakeBackend {
                shared: Arc::clone(shared),
                armed: Some(self.armed),
                rescue: self.rescue,
            }))
        }
    }

    struct FailedWakeBackend {
        shared: Arc<Shared>,
        armed: Option<mpsc::Sender<()>>,
        rescue: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Backend for FailedWakeBackend {
        fn submit(
            &mut self,
            _id: RequestId,
            _request: Request,
            _accepted_at: Instant,
        ) -> Option<Completion> {
            None
        }

        fn cancel(&mut self, _id: RequestId) {}

        fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining > Duration::from_millis(10) {
                if let Some(armed) = self.armed.take() {
                    self.shared.queue.set_external_waker(Some(Arc::new(|| {
                        Err(Error::new(
                            ErrorKind::Internal,
                            "deliberate native wake failure",
                        ))
                    })));
                    let _send_result = armed.send(());
                }
                let (lock, changed) = &*self.rescue;
                let rescued = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let _wait_result = changed
                    .wait_timeout_while(rescued, remaining, |rescued| !*rescued)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            Ok(Vec::new())
        }

        fn shutdown(&mut self) -> Result<(), ShutdownError> {
            Ok(())
        }

        fn poll_mode(&self) -> PollMode {
            PollMode::Interruptible {
                max_wait: NATIVE_SAFETY_POLL,
            }
        }
    }

    struct RawBackend {
        reactor: NativeReactor,
        request_to_slot: HashMap<RequestId, SlotId>,
        slot_to_request: HashMap<SlotId, RequestId>,
        bodies: HashMap<RequestId, Vec<u8>>,
    }

    impl RawBackend {
        fn new() -> Result<Self, Error> {
            Ok(Self {
                reactor: NativeReactor::new(32).map_err(native_internal_error)?,
                request_to_slot: HashMap::new(),
                slot_to_request: HashMap::new(),
                bodies: HashMap::new(),
            })
        }

        fn remove_slot(&mut self, slot: SlotId) -> Option<RequestId> {
            let request_id = self.slot_to_request.remove(&slot)?;
            self.request_to_slot.remove(&request_id);
            Some(request_id)
        }
    }

    impl Backend for RawBackend {
        fn submit(
            &mut self,
            id: RequestId,
            request: Request,
            accepted_at: Instant,
        ) -> Option<Completion> {
            let origin = match http_origin(request.url(), ErrorKind::InvalidRequest) {
                Ok(origin) => origin,
                Err(error) => return Some(Completion::Failed(error)),
            };
            let ip = match origin.host.parse::<IpAddr>() {
                Ok(ip) => ip,
                Err(_) => {
                    return Some(Completion::Failed(Error::new(
                        ErrorKind::Unsupported,
                        "raw native fixture accepts literal IP addresses only",
                    )));
                }
            };
            let deadline = request
                .options()
                .total_timeout
                .and_then(|timeout| accepted_at.checked_add(timeout));
            let slot = match self.reactor.connect(
                SocketAddr::new(ip, origin.port),
                deadline,
                TEST_LIMIT,
                TEST_LIMIT,
            ) {
                Ok(slot) => slot,
                Err(failure) => {
                    return Some(Completion::Failed(native_transport_error(failure)));
                }
            };
            if let Err(failure) = self.reactor.queue_write(slot, request.body()) {
                self.reactor.cancel(slot);
                return Some(Completion::Failed(native_transport_error(failure)));
            }
            self.request_to_slot.insert(id, slot);
            self.slot_to_request.insert(slot, id);
            self.bodies.insert(id, Vec::new());
            None
        }

        fn cancel(&mut self, id: RequestId) {
            if let Some(slot) = self.request_to_slot.remove(&id) {
                self.slot_to_request.remove(&slot);
                self.bodies.remove(&id);
                self.reactor.cancel(slot);
            }
        }

        fn poll(&mut self, deadline: Instant) -> Result<Vec<BackendCompletion>, Error> {
            let events = self.reactor.poll(deadline).map_err(native_internal_error)?;
            let mut completions = Vec::new();
            for event in events {
                match event {
                    NativeEvent::Connected(_)
                    | NativeEvent::WriteProgress(_)
                    | NativeEvent::WriteDrained(_) => {}
                    NativeEvent::Data(slot, bytes) => {
                        if let Some(request_id) = self.slot_to_request.get(&slot) {
                            if let Some(body) = self.bodies.get_mut(request_id) {
                                body.extend(bytes);
                            }
                        }
                    }
                    NativeEvent::PeerClosed(slot) => {
                        if let Some(request_id) = self.remove_slot(slot) {
                            self.reactor.cancel(slot);
                            let body = self.bodies.remove(&request_id).unwrap_or_default();
                            completions.push(BackendCompletion {
                                id: request_id,
                                completion: Completion::Completed(Response::new(200, vec![], body)),
                            });
                        }
                    }
                    NativeEvent::Failed(slot, failure) => {
                        if let Some(request_id) = self.remove_slot(slot) {
                            self.bodies.remove(&request_id);
                            completions.push(BackendCompletion {
                                id: request_id,
                                completion: Completion::Failed(native_transport_error(failure)),
                            });
                        }
                    }
                    NativeEvent::DeadlineExpired(slot) => {
                        if let Some(request_id) = self.remove_slot(slot) {
                            self.reactor.cancel(slot);
                            self.bodies.remove(&request_id);
                            completions.push(BackendCompletion {
                                id: request_id,
                                completion: Completion::Failed(Error::timeout(
                                    TimeoutKind::Total,
                                    "raw native fixture deadline expired",
                                )),
                            });
                        }
                    }
                }
            }
            Ok(completions)
        }

        fn shutdown(&mut self) -> Result<(), ShutdownError> {
            self.request_to_slot.clear();
            self.slot_to_request.clear();
            self.bodies.clear();
            self.reactor.shutdown();
            Ok(())
        }

        fn poll_mode(&self) -> PollMode {
            PollMode::Interruptible {
                max_wait: NATIVE_SAFETY_POLL,
            }
        }
    }

    fn native_internal_error(failure: NativeFailure) -> Error {
        Error::new(ErrorKind::Internal, failure.message)
    }

    fn native_transport_error(failure: NativeFailure) -> Error {
        let stage = match failure.kind {
            NativeFailureKind::Connect => TransportStage::Connect,
            NativeFailureKind::Write => TransportStage::Send,
            NativeFailureKind::Read => TransportStage::Receive,
            NativeFailureKind::OutboundQueueFull => TransportStage::Send,
            NativeFailureKind::ReceiveLimit => TransportStage::Receive,
            NativeFailureKind::Internal => TransportStage::Receive,
        };
        Error::transport(stage, failure.message)
    }

    fn listener() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener must bind");
        let address = listener
            .local_addr()
            .expect("loopback listener must have an address");
        (listener, address)
    }

    fn drive_until(
        reactor: &mut NativeReactor,
        timeout: Duration,
        mut done: impl FnMut(&[NativeEvent]) -> bool,
    ) -> Vec<NativeEvent> {
        let deadline = Instant::now() + timeout;
        let mut all = Vec::new();
        while Instant::now() < deadline {
            let events = reactor.poll(deadline).expect("native poll must succeed");
            all.extend(events);
            if done(&all) {
                return all;
            }
        }
        panic!("native reactor fixture timed out: {all:?}");
    }

    #[test]
    fn spawned_engine_uses_native_wakeup_for_completion_and_cancellation() {
        let (listener, address) = listener();
        let (first_read_tx, first_read_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("first request must connect");
            let mut request = [0_u8; 4];
            first
                .read_exact(&mut request)
                .expect("first body must arrive");
            first_read_tx
                .send(request)
                .expect("test receiver must remain");
            release_rx.recv().expect("test release must arrive");

            let (mut second, _) = listener.accept().expect("second request must connect");
            second
                .read_exact(&mut request)
                .expect("second body must arrive");
            second.write_all(b"pong").expect("response must write");
        });

        let engine = Engine::with_spawned_factory(EngineConfig::spawned(), Box::new(RawFactory))
            .expect("native fixture Engine must construct");
        let client = engine.client();
        let first = client
            .submit(
                Request::post(format!("http://{address}/"))
                    .body(b"ping".to_vec())
                    .build()
                    .expect("first request must build"),
            )
            .expect("first request must submit");
        assert_eq!(
            first_read_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("native write must wake and arrive"),
            *b"ping"
        );
        let cancel_started = Instant::now();
        first.handle().cancel().expect("cancel must succeed");
        assert!(matches!(first.wait(), Completion::Cancelled));
        assert!(
            cancel_started.elapsed() < Duration::from_millis(500),
            "native cancellation did not wake promptly"
        );
        release_tx.send(()).expect("server release must send");

        let response = client
            .execute(
                Request::post(format!("http://{address}/"))
                    .body(b"ping".to_vec())
                    .build()
                    .expect("second request must build"),
            )
            .expect("second request must complete");
        assert_eq!(response.body(), b"pong");
        let shutdown_started = Instant::now();
        engine.shutdown().expect("native Engine must join");
        assert!(
            shutdown_started.elapsed() < Duration::from_millis(500),
            "native shutdown did not wake and join promptly"
        );
        server.join().expect("server must join");
    }

    #[test]
    fn active_manual_native_engine_moves_between_threads() {
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request must connect");
            let mut request = [0_u8; 4];
            stream.read_exact(&mut request).expect("body must arrive");
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").expect("response must write");
        });
        let engine = Engine::with_backend(
            EngineConfig::manual(),
            Box::new(RawBackend::new().expect("raw backend must construct")),
        )
        .expect("manual native fixture Engine must construct");
        let pending = engine
            .client()
            .submit(
                Request::post(format!("http://{address}/"))
                    .body(b"ping".to_vec())
                    .build()
                    .expect("request must build"),
            )
            .expect("request must submit");
        let owner = thread::spawn(move || {
            let mut engine = engine;
            let completion = engine
                .drive_until(pending)
                .expect("moved manual Engine must drive");
            engine.shutdown().expect("moved manual Engine must stop");
            completion
        });
        let completion = owner.join().expect("manual owner must join");
        let Completion::Completed(response) = completion else {
            panic!("manual native transfer did not complete");
        };
        assert_eq!(response.body(), b"pong");
        server.join().expect("server must join");
    }

    #[test]
    fn fragmented_echo_and_half_close_are_reported() {
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("server must accept");
            let mut request = [0_u8; 6];
            stream.read_exact(&mut request).expect("server must read");
            assert_eq!(&request, b"abcdef");
            stream.write_all(b"ab").expect("first fragment must write");
            thread::sleep(Duration::from_millis(10));
            stream
                .write_all(b"cdef")
                .expect("second fragment must write");
            stream
                .shutdown(Shutdown::Write)
                .expect("server must half-close");
        });

        let mut reactor = NativeReactor::new(16).expect("reactor must construct");
        let id = reactor
            .connect(address, None, TEST_LIMIT, TEST_LIMIT)
            .expect("connect must start");
        reactor
            .queue_write(id, b"abcdef")
            .expect("write must queue");
        let events = drive_until(&mut reactor, Duration::from_secs(2), |events| {
            events.contains(&NativeEvent::PeerClosed(id))
        });
        let received = events
            .iter()
            .filter_map(|event| match event {
                NativeEvent::Data(event_id, bytes) if *event_id == id => Some(bytes.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(received, b"abcdef");
        assert!(events.contains(&NativeEvent::Connected(id)));
        assert!(events.contains(&NativeEvent::WriteDrained(id)));
        assert_eq!(reactor.active_count(), 1);
        assert!(reactor.cancel(id));
        assert_eq!(reactor.active_count(), 0);
        server.join().expect("server must join");
    }

    #[test]
    fn peer_half_close_keeps_the_write_half_available() {
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("server must accept");
            stream.write_all(b"done").expect("server must write");
            stream
                .shutdown(Shutdown::Write)
                .expect("server must half-close");
            let mut reply = [0_u8; 9];
            stream
                .read_exact(&mut reply)
                .expect("server must retain its read half");
            assert_eq!(&reply, b"after-fin");
        });

        let mut reactor = NativeReactor::new(8).expect("reactor must construct");
        let id = reactor
            .connect(address, None, TEST_LIMIT, TEST_LIMIT)
            .expect("connect must start");
        let events = drive_until(&mut reactor, Duration::from_secs(2), |events| {
            events.contains(&NativeEvent::PeerClosed(id))
        });
        assert!(events.iter().any(
            |event| matches!(event, NativeEvent::Data(event_id, bytes) if *event_id == id && bytes == b"done")
        ));
        assert_eq!(reactor.active_count(), 1);

        reactor
            .queue_write(id, b"after-fin")
            .expect("local write half must remain usable");
        let events = drive_until(&mut reactor, Duration::from_secs(2), |events| {
            events.contains(&NativeEvent::WriteDrained(id))
        });
        assert!(events.contains(&NativeEvent::WriteDrained(id)));
        assert!(reactor.cancel(id));
        server.join().expect("server must join");
    }

    #[test]
    fn cancellation_releases_slot_and_generation_rejects_stale_id() {
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let (_first, _) = listener.accept().expect("first connect must arrive");
            let (_second, _) = listener.accept().expect("second connect must arrive");
        });
        let mut reactor = NativeReactor::new(8).expect("reactor must construct");
        let first = reactor
            .connect(address, None, 32, 32)
            .expect("first connect must start");
        assert!(reactor.cancel(first));
        let second = reactor
            .connect(address, None, 32, 32)
            .expect("second connect must start");
        assert_eq!(first.index, second.index);
        assert_ne!(first.generation, second.generation);
        assert!(!reactor.cancel(first));
        assert_eq!(reactor.active_count(), 1);
        assert!(reactor.cancel(second));
        server.join().expect("server must join");
    }

    #[test]
    fn waker_interrupts_a_long_poll() {
        let mut reactor = NativeReactor::new(4).expect("reactor must construct");
        let waker = reactor.waker();
        let poller = thread::spawn(move || {
            let started = Instant::now();
            let events = reactor
                .poll(Instant::now() + Duration::from_secs(30))
                .expect("poll must wake");
            (started.elapsed(), events)
        });
        thread::sleep(Duration::from_millis(20));
        waker.wake().expect("waker must notify");
        let (elapsed, events) = poller.join().expect("poller must join");
        assert!(
            elapsed < Duration::from_millis(500),
            "wake took {elapsed:?}"
        );
        assert!(events.is_empty());
    }

    #[test]
    fn spawned_native_backend_has_a_short_safety_poll() {
        let backend = RawBackend::new().expect("raw backend must construct");
        assert_eq!(
            backend.poll_mode(),
            PollMode::Interruptible {
                max_wait: Duration::from_millis(50),
            }
        );
    }

    #[test]
    fn safety_poll_recovers_when_external_wake_fails() {
        let (armed_tx, armed_rx) = mpsc::channel();
        let rescue = Arc::new((Mutex::new(false), Condvar::new()));
        let engine = Engine::with_spawned_factory(
            EngineConfig::spawned(),
            Box::new(FailedWakeFactory {
                armed: armed_tx,
                rescue: Arc::clone(&rescue),
            }),
        )
        .expect("failed-wake fixture Engine must construct");
        let client = engine.client();
        let first = client
            .submit(
                Request::get("http://127.0.0.1:1/")
                    .build()
                    .expect("first request must build"),
            )
            .expect("first request must submit");
        armed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("backend must enter its interruptible wait");

        let started = Instant::now();
        let second = client
            .submit(
                Request::get("http://127.0.0.1:1/")
                    .build()
                    .expect("second request must build"),
            )
            .expect("second request must submit");
        let (second_completion, needed_rescue) = match second.wait_for(Duration::from_millis(500)) {
            WaitOutcome::Completed(completion) => (completion, false),
            WaitOutcome::TimedOut(pending) => {
                let (lock, changed) = &*rescue;
                *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
                changed.notify_all();
                (pending.wait(), true)
            }
        };
        let first_completion = first.wait();
        let shutdown = engine
            .shutdown()
            .expect_err("latched wake failure must remain observable at shutdown");

        assert!(
            !needed_rescue,
            "failed wake exceeded the 500 ms safety gate"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(matches!(
            first_completion,
            Completion::Failed(ref error) if error.kind() == ErrorKind::Internal
        ));
        assert!(matches!(
            second_completion,
            Completion::Failed(ref error) if error.kind() == ErrorKind::Internal
        ));
        assert_eq!(shutdown.error().kind(), ErrorKind::Internal);
    }

    #[test]
    fn deadline_closes_an_active_connection() {
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("server must accept");
            thread::sleep(Duration::from_millis(200));
        });
        let mut reactor = NativeReactor::new(8).expect("reactor must construct");
        let id = reactor
            .connect(
                address,
                Some(Instant::now() + Duration::from_millis(40)),
                32,
                32,
            )
            .expect("connect must start");
        let events = drive_until(&mut reactor, Duration::from_secs(1), |events| {
            events.contains(&NativeEvent::DeadlineExpired(id))
        });
        assert!(events.contains(&NativeEvent::DeadlineExpired(id)));
        assert_eq!(reactor.active_count(), 1);
        assert!(reactor.cancel(id));
        assert_eq!(reactor.active_count(), 0);
        server.join().expect("server must join");
    }

    #[test]
    fn abortive_close_is_reported_and_released() {
        let (listener, address) = listener();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (reset_tx, reset_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("server must accept");
            accepted_tx.send(()).expect("accept signal must send");
            reset_rx.recv().expect("reset signal must arrive");
            let socket = socket2::Socket::from(stream);
            socket
                .set_linger(Some(Duration::ZERO))
                .expect("abortive linger must configure");
            drop(socket);
        });
        let mut reactor = NativeReactor::new(8).expect("reactor must construct");
        let id = reactor
            .connect(
                address,
                Some(Instant::now() + Duration::from_secs(2)),
                32,
                32,
            )
            .expect("connect must start");
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server must accept promptly");
        drive_until(&mut reactor, Duration::from_secs(1), |events| {
            events.contains(&NativeEvent::Connected(id))
        });
        reset_tx.send(()).expect("reset signal must send");
        let events = drive_until(&mut reactor, Duration::from_secs(1), |events| {
            events.iter().any(|event| {
                matches!(event, NativeEvent::Failed(event_id, failure)
                    if *event_id == id && failure.kind == NativeFailureKind::Read)
            })
        });
        assert!(events.iter().any(|event| {
            matches!(event, NativeEvent::Failed(event_id, failure)
                if *event_id == id && failure.kind == NativeFailureKind::Read)
        }));
        assert_eq!(reactor.active_count(), 0);
        server.join().expect("server must join");
    }

    #[test]
    fn bounds_are_checked_before_queue_growth() {
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("server must accept");
            stream.write_all(b"12345").expect("server must write");
        });
        let mut reactor = NativeReactor::new(8).expect("reactor must construct");
        let id = reactor
            .connect(address, None, 4, 4)
            .expect("connect must start");
        let queue_error = reactor
            .queue_write(id, b"12345")
            .expect_err("outbound limit must reject");
        assert_eq!(queue_error.kind, NativeFailureKind::OutboundQueueFull);
        let events = drive_until(&mut reactor, Duration::from_secs(1), |events| {
            events.iter().any(|event| {
                matches!(event, NativeEvent::Failed(event_id, failure)
                    if *event_id == id && failure.kind == NativeFailureKind::ReceiveLimit)
            })
        });
        assert!(events.iter().any(|event| {
            matches!(event, NativeEvent::Failed(event_id, failure)
                if *event_id == id && failure.kind == NativeFailureKind::ReceiveLimit)
        }));
        server.join().expect("server must join");
    }

    #[test]
    fn many_connections_progress_with_a_small_event_buffer() {
        const CONNECTIONS: usize = 32;
        let (listener, address) = listener();
        let server = thread::spawn(move || {
            for _ in 0..CONNECTIONS {
                let (mut stream, _) = listener.accept().expect("server must accept every slot");
                let mut byte = [0_u8; 1];
                stream.read_exact(&mut byte).expect("server must read byte");
                stream.write_all(&byte).expect("server must echo byte");
            }
        });
        let mut reactor = NativeReactor::new(4).expect("reactor must construct");
        let mut expected = HashMap::new();
        for value in 0..CONNECTIONS {
            let id = reactor
                .connect(address, None, 1, 1)
                .expect("connection must start");
            reactor
                .queue_write(id, &[value as u8])
                .expect("byte must queue");
            expected.insert(id, value as u8);
        }
        let events = drive_until(&mut reactor, Duration::from_secs(3), |events| {
            events
                .iter()
                .filter(|event| matches!(event, NativeEvent::PeerClosed(_)))
                .count()
                == CONNECTIONS
        });
        let received = events
            .iter()
            .filter_map(|event| match event {
                NativeEvent::Data(id, bytes) if bytes.len() == 1 => Some((*id, bytes[0])),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(received, expected);
        assert_eq!(reactor.active_count(), CONNECTIONS);
        for id in expected.keys().copied() {
            assert!(reactor.cancel(id));
        }
        assert_eq!(reactor.active_count(), 0);
        server.join().expect("server must join");
    }

    #[test]
    fn shutdown_is_idempotent_and_reactor_is_send() {
        fn require_send<T: Send>() {}
        require_send::<NativeReactor>();
        require_send::<NativeWaker>();

        for _ in 0..100 {
            let mut reactor = NativeReactor::new(1).expect("reactor must construct");
            reactor.shutdown();
            reactor.shutdown();
            assert_eq!(reactor.active_count(), 0);
        }
    }
}
