//! Cross-platform native readiness with a WinSock fallback for older Wine.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use mio::event::Source;
use mio::{Events, Interest, Poll, Token, Waker};

#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

#[cfg(windows)]
const WINSOCK_SAFETY_POLL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub(super) struct NativeWaker {
    inner: Arc<Mutex<Option<Waker>>>,
}

impl NativeWaker {
    pub(super) fn wake(&self) -> io::Result<()> {
        match lock_unpoisoned(&self.inner).as_ref() {
            Some(waker) => waker.wake(),
            None => Ok(()),
        }
    }

    #[cfg(windows)]
    fn disable_mio(&self) {
        lock_unpoisoned(&self.inner).take();
    }

    #[cfg(all(test, windows))]
    fn mio_is_enabled(&self) -> bool {
        lock_unpoisoned(&self.inner).is_some()
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) struct PollTarget {
    token: Token,
    readable: bool,
    writable: bool,
    #[cfg(windows)]
    socket: usize,
}

impl PollTarget {
    pub(super) fn new<S: NativeSource>(token: Token, source: &S, interest: Interest) -> Self {
        #[cfg(not(windows))]
        let _ = source;
        Self {
            token,
            readable: interest.is_readable(),
            writable: interest.is_writable(),
            #[cfg(windows)]
            socket: source.as_raw_socket() as usize,
        }
    }
}

#[cfg(windows)]
pub(super) trait NativeSource: Source + AsRawSocket {}

#[cfg(windows)]
impl<T: Source + AsRawSocket> NativeSource for T {}

#[cfg(not(windows))]
pub(super) trait NativeSource: Source {}

#[cfg(not(windows))]
impl<T: Source> NativeSource for T {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PollReady {
    pub(super) token: Token,
    pub(super) readable: bool,
    pub(super) writable: bool,
    pub(super) error: bool,
}

pub(super) struct NativePoll {
    implementation: PollImplementation,
    #[cfg(windows)]
    waker: NativeWaker,
    registered: usize,
}

enum PollImplementation {
    Mio {
        poll: Poll,
        events: Events,
    },
    #[cfg(windows)]
    WinSock,
}

impl NativePoll {
    pub(super) fn new(event_capacity: usize, wake_token: Token) -> io::Result<(Self, NativeWaker)> {
        let poll = Poll::new()?;
        let waker = NativeWaker {
            inner: Arc::new(Mutex::new(Some(Waker::new(poll.registry(), wake_token)?))),
        };
        Ok((
            Self {
                implementation: PollImplementation::Mio {
                    poll,
                    events: Events::with_capacity(event_capacity.max(1)),
                },
                #[cfg(windows)]
                waker: waker.clone(),
                registered: 0,
            },
            waker,
        ))
    }

    pub(super) fn register<S: NativeSource>(
        &mut self,
        source: &mut S,
        token: Token,
        interest: Interest,
    ) -> io::Result<()> {
        match &mut self.implementation {
            PollImplementation::Mio { poll, .. } => {
                match poll.registry().register(source, token, interest) {
                    Ok(()) => {
                        self.registered = self.registered.saturating_add(1);
                        Ok(())
                    }
                    #[cfg(windows)]
                    Err(error) if self.registered == 0 && afd_is_unavailable(&error) => {
                        // Wait for any in-flight wake, then drop the completion-port waker before
                        // dropping Mio's Poll. Every clone becomes a successful no-op so an old
                        // Wine process cannot accumulate completions that nobody will drain.
                        self.waker.disable_mio();
                        self.implementation = PollImplementation::WinSock;
                        self.registered = 1;
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            #[cfg(windows)]
            PollImplementation::WinSock => {
                self.registered = self.registered.saturating_add(1);
                Ok(())
            }
        }
    }

    pub(super) fn reregister<S: NativeSource>(
        &mut self,
        source: &mut S,
        token: Token,
        interest: Interest,
    ) -> io::Result<()> {
        match &mut self.implementation {
            PollImplementation::Mio { poll, .. } => {
                poll.registry().reregister(source, token, interest)
            }
            #[cfg(windows)]
            PollImplementation::WinSock => Ok(()),
        }
    }

    pub(super) fn deregister<S: NativeSource>(&mut self, source: &mut S) -> io::Result<()> {
        let result = match &mut self.implementation {
            PollImplementation::Mio { poll, .. } => poll.registry().deregister(source),
            #[cfg(windows)]
            PollImplementation::WinSock => Ok(()),
        };
        if result.is_ok() {
            self.registered = self.registered.saturating_sub(1);
        }
        result
    }

    pub(super) fn poll(
        &mut self,
        targets: &[PollTarget],
        timeout: Duration,
        wake_token: Token,
    ) -> io::Result<Vec<PollReady>> {
        #[cfg(not(windows))]
        let _ = targets;
        match &mut self.implementation {
            PollImplementation::Mio { poll, events } => {
                poll.poll(events, Some(timeout))?;
                Ok(events
                    .iter()
                    .filter(|event| event.token() != wake_token)
                    .map(|event| PollReady {
                        token: event.token(),
                        readable: event.is_readable() || event.is_read_closed() || event.is_error(),
                        writable: event.is_writable()
                            || event.is_write_closed()
                            || event.is_error(),
                        error: event.is_error(),
                    })
                    .collect())
            }
            #[cfg(windows)]
            PollImplementation::WinSock => {
                let targets = targets
                    .iter()
                    .map(|target| nbreq_winpoll::PollTarget {
                        key: target.token.0,
                        socket: target.socket,
                        readable: target.readable,
                        writable: target.writable,
                    })
                    .collect::<Vec<_>>();
                // The Mio completion-port waker is deliberately passive after falling back.
                // Bounding the documented WSAPoll call preserves prompt submit/cancel/shutdown
                // even if a command arrives just after the target list was captured.
                Ok(
                    nbreq_winpoll::poll(&targets, timeout.min(WINSOCK_SAFETY_POLL))?
                        .into_iter()
                        .map(|event| PollReady {
                            token: Token(event.key),
                            readable: event.readable,
                            writable: event.writable,
                            error: event.error,
                        })
                        .collect(),
                )
            }
        }
    }

    #[cfg(all(test, windows))]
    pub(super) fn force_winsock(&mut self) {
        debug_assert_eq!(self.registered, 0);
        self.waker.disable_mio();
        self.implementation = PollImplementation::WinSock;
    }
}

#[cfg(windows)]
fn afd_is_unavailable(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound && error.to_string().contains("\\Device\\Afd")
}

#[cfg(all(test, windows))]
mod tests {
    use super::{NativePoll, afd_is_unavailable};
    use mio::Token;
    use std::io;

    #[test]
    fn fallback_is_narrowly_limited_to_missing_afd() {
        let missing = io::Error::new(
            io::ErrorKind::NotFound,
            "Failed to open \\Device\\Afd\\Mio: Path not found. (os error 3)",
        );
        assert!(afd_is_unavailable(&missing));
        assert!(!afd_is_unavailable(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Failed to open \\Device\\Afd\\Mio",
        )));
        assert!(!afd_is_unavailable(&io::Error::new(
            io::ErrorKind::NotFound,
            "some unrelated path was not found",
        )));
    }

    #[test]
    fn fallback_turns_every_existing_waker_clone_into_a_no_op() {
        let (mut poll, waker) = NativePoll::new(4, Token(0)).expect("poll must construct");
        let clone = waker.clone();
        assert!(waker.mio_is_enabled());

        poll.force_winsock();

        assert!(!waker.mio_is_enabled());
        for _ in 0..10_000 {
            waker.wake().expect("disabled wake must be a no-op");
            clone.wake().expect("existing clone must also be a no-op");
        }
    }
}
