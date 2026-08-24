//! MSRV-compatible atomic update loops.
//!
//! Rust 1.99 deprecates `Atomic*::fetch_update` in favour of `try_update`, but `try_update` was not
//! stabilized until Rust 1.95. NBReq supports Rust 1.85, so these small compare/exchange loops keep
//! one implementation that is warning-free on both sides of that transition.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub(crate) fn try_update_u64(
    atomic: &AtomicU64,
    set_order: Ordering,
    fetch_order: Ordering,
    mut update: impl FnMut(u64) -> Option<u64>,
) -> Result<u64, u64> {
    let mut current = atomic.load(fetch_order);
    loop {
        let Some(next) = update(current) else {
            return Err(current);
        };
        match atomic.compare_exchange_weak(current, next, set_order, fetch_order) {
            Ok(previous) => return Ok(previous),
            Err(observed) => current = observed,
        }
    }
}

pub(crate) fn try_update_usize(
    atomic: &AtomicUsize,
    set_order: Ordering,
    fetch_order: Ordering,
    mut update: impl FnMut(usize) -> Option<usize>,
) -> Result<usize, usize> {
    let mut current = atomic.load(fetch_order);
    loop {
        let Some(next) = update(current) else {
            return Err(current);
        };
        match atomic.compare_exchange_weak(current, next, set_order, fetch_order) {
            Ok(previous) => return Ok(previous),
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    #[test]
    fn update_loop_retries_under_contention_and_preserves_none_semantics() {
        let value = Arc::new(AtomicUsize::new(0));
        let workers = (0..8)
            .map(|_| {
                let value = Arc::clone(&value);
                thread::spawn(move || {
                    for _ in 0..10_000 {
                        try_update_usize(&value, Ordering::AcqRel, Ordering::Acquire, |current| {
                            Some(current + 1)
                        })
                        .expect("increment update must succeed");
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("update worker must not panic");
        }
        assert_eq!(value.load(Ordering::Acquire), 80_000);
        assert_eq!(
            try_update_usize(&value, Ordering::AcqRel, Ordering::Acquire, |_| None),
            Err(80_000)
        );

        let wide = AtomicU64::new(u64::MAX);
        assert_eq!(
            try_update_u64(&wide, Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_add(1))
            }),
            Ok(u64::MAX)
        );
        assert_eq!(wide.load(Ordering::Acquire), u64::MAX);
    }
}
