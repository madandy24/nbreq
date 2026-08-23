//! A tiny safe wrapper around WinSock's documented `WSAPoll` API.
//!
//! NBReq normally uses Mio. Older Wine releases do not implement the private AFD object Mio uses,
//! so Windows binaries need this deliberately boring compatibility path. Keeping the FFI in this
//! private crate preserves `#![forbid(unsafe_code)]` in NBReq proper and makes the unsafe surface
//! small enough to audit in one screen.

#![cfg(windows)]

use std::io;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Networking::WinSock::{
    POLLERR, POLLHUP, POLLNVAL, POLLRDBAND, POLLRDNORM, POLLWRNORM, SOCKET_ERROR, WSAGetLastError,
    WSAPOLLFD, WSAPoll,
};

/// One socket and its requested readiness interests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PollTarget {
    /// Caller-owned stable key returned in [`PollEvent`].
    pub key: usize,
    /// WinSock `SOCKET` value, obtained from `AsRawSocket`.
    pub socket: usize,
    /// Observe readable data, FIN, and receive errors.
    pub readable: bool,
    /// Observe write capacity and connect completion.
    pub writable: bool,
}

/// Readiness returned for one target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PollEvent {
    /// Stable key supplied in [`PollTarget`].
    pub key: usize,
    /// Data or FIN may be observed without blocking.
    pub readable: bool,
    /// A write or nonblocking-connect status check may make progress.
    pub writable: bool,
    /// WinSock reported an error, hangup, or invalid socket.
    pub error: bool,
}

/// Waits for readiness without retaining pointers or socket ownership.
pub fn poll(targets: &[PollTarget], timeout: Duration) -> io::Result<Vec<PollEvent>> {
    if targets.is_empty() {
        thread::sleep(timeout);
        return Ok(Vec::new());
    }
    let count = u32::try_from(targets.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many sockets for WSAPoll"))?;
    let mut descriptors = targets
        .iter()
        .map(|target| WSAPOLLFD {
            fd: target.socket,
            events: requested_events(*target),
            revents: 0,
        })
        .collect::<Vec<_>>();
    let timeout_ms = duration_to_timeout_ms(timeout);

    // SAFETY: `descriptors` is a live, writable array of exactly `count` WSAPOLLFD values for the
    // duration of the call. WSAPoll does not retain the pointer. Socket values came from
    // `AsRawSocket` and their owners remain alive in the calling reactor while this function runs.
    let result = unsafe { WSAPoll(descriptors.as_mut_ptr(), count, timeout_ms) };
    if result == SOCKET_ERROR {
        // SAFETY: WSAGetLastError has no arguments and reads thread-local WinSock error state.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }

    Ok(descriptors
        .iter()
        .zip(targets)
        .filter(|(descriptor, _target)| descriptor.revents != 0)
        .map(|(descriptor, target)| {
            let error = descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0;
            PollEvent {
                key: target.key,
                // A hangup must drive a final recv so the owner observes FIN after retained
                // plaintext. Error readiness drives both sides so connect/read/write status
                // is classified by the existing socket-owner code.
                readable: error || descriptor.revents & (POLLRDNORM | POLLRDBAND) != 0,
                writable: error || descriptor.revents & POLLWRNORM != 0,
                error,
            }
        })
        .collect())
}

fn requested_events(target: PollTarget) -> i16 {
    let mut events = 0;
    if target.readable {
        events |= POLLRDNORM | POLLRDBAND;
    }
    if target.writable {
        events |= POLLWRNORM;
    }
    events
}

fn duration_to_timeout_ms(timeout: Duration) -> i32 {
    if timeout.is_zero() {
        return 0;
    }
    let millis = timeout.as_millis().max(1).min(i32::MAX as u128);
    millis as i32
}

#[cfg(test)]
mod tests {
    use super::duration_to_timeout_ms;
    use std::time::Duration;

    #[test]
    fn timeout_rounding_is_bounded_and_never_turns_a_positive_wait_into_a_poll() {
        assert_eq!(duration_to_timeout_ms(Duration::ZERO), 0);
        assert_eq!(duration_to_timeout_ms(Duration::from_nanos(1)), 1);
        assert_eq!(duration_to_timeout_ms(Duration::from_millis(50)), 50);
        assert_eq!(duration_to_timeout_ms(Duration::MAX), i32::MAX);
    }
}
