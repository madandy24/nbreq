//! Cross-platform native readiness with a WinSock fallback for older Wine.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use mio::event::Source;
use mio::{Events, Interest, Poll, Token, Waker};

#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

#[cfg(windows)]
const WINSOCK_SAFETY_POLL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub(super) struct NativeWaker {
    inner: Arc<Waker>,
}

impl NativeWaker {
    pub(super) fn wake(&self) -> io::Result<()> {
        self.inner.wake()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PollTarget {
    token: Token,
    readable: bool,
    writable: bool,
    #[cfg(windows)]
    socket: usize,
}

impl PollTarget {
    pub(super) fn new<S: NativeSource>(token: Token, source: &S, interest: Interest) -> Self {
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
            inner: Arc::new(Waker::new(poll.registry(), wake_token)?),
        };
        Ok((
            Self {
                implementation: PollImplementation::Mio {
                    poll,
                    events: Events::with_capacity(event_capacity.max(1)),
                },
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
        self.implementation = PollImplementation::WinSock;
    }
}

#[cfg(windows)]
fn afd_is_unavailable(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound && error.to_string().contains("\\Device\\Afd")
}

#[cfg(all(test, windows))]
mod tests {
    use super::afd_is_unavailable;
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
}
