//! Instrumentation only: requested Rust heap sizes, not allocator footprint or process RAM.
//!
//! Accounting is linearized under a nonallocating lock, after successful allocation/reallocation
//! and before deallocation. Reallocation records the replacement logical allocation; allocator
//! implementation transients are outside this measure. Never use instrumented timings as plain
//! performance results. This unpublished crate is not a dependency of NBReq.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub live_bytes: usize,
    pub peak_live_bytes: usize,
    pub allocations: usize,
    pub deallocations: usize,
    pub reallocations: usize,
    pub bytes_requested: usize,
    pub bytes_released: usize,
}

struct Counters {
    locked: AtomicBool,
    values: [AtomicUsize; 7],
}

struct Guard<'a>(&'a AtomicBool);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Counters {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            values: [const { AtomicUsize::new(0) }; 7],
        }
    }

    fn lock(&self) -> Guard<'_> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        Guard(&self.locked)
    }

    fn snapshot(&self, reset_peak: bool) -> Snapshot {
        let _guard = self.lock();
        let [
            live_bytes,
            mut peak_live_bytes,
            allocations,
            deallocations,
            reallocations,
            bytes_requested,
            bytes_released,
        ] = self
            .values
            .each_ref()
            .map(|value| value.load(Ordering::Relaxed));
        if reset_peak {
            peak_live_bytes = live_bytes;
            self.values[1].store(live_bytes, Ordering::Relaxed);
        }
        Snapshot {
            live_bytes,
            peak_live_bytes,
            allocations,
            deallocations,
            reallocations,
            bytes_requested,
            bytes_released,
        }
    }

    fn change(&self, old: usize, new: usize, kind: usize) {
        let _guard = self.lock();
        let live = self.values[0]
            .load(Ordering::Relaxed)
            .wrapping_sub(old)
            .wrapping_add(new);
        self.values[0].store(live, Ordering::Relaxed);
        self.values[1].fetch_max(live, Ordering::Relaxed);
        self.values[kind].fetch_add(1, Ordering::Relaxed);
        self.values[5].fetch_add(new, Ordering::Relaxed);
        self.values[6].fetch_add(old, Ordering::Relaxed);
    }
}

pub struct Metered<A> {
    inner: A,
    counters: Counters,
}

impl<A> Metered<A> {
    pub const fn new(inner: A) -> Self {
        Self {
            inner,
            counters: Counters::new(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.counters.snapshot(false)
    }

    /// Reset only the phase peak to current live bytes. Counters and live ownership never reset.
    pub fn begin_phase(&self) -> Snapshot {
        self.counters.snapshot(true)
    }
}

// SAFETY: Allocation ownership, layout, alignment and failure behavior are delegated unchanged
// to A. Bookkeeping allocates nothing, never invokes user code, and never holds its lock while
// calling A. Each successful logical allocation is recorded before its pointer is returned.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Metered<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller supplies GlobalAlloc's valid layout; A receives it unchanged.
        let pointer = unsafe { self.inner.alloc(layout) };
        if !pointer.is_null() {
            self.counters.change(0, layout.size(), 2);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller's layout is forwarded unchanged, including zero-initialization.
        let pointer = unsafe { self.inner.alloc_zeroed(layout) };
        if !pointer.is_null() {
            self.counters.change(0, layout.size(), 2);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        self.counters.change(layout.size(), 0, 3);
        // SAFETY: The caller owns this allocation from A, with the matching original layout.
        unsafe { self.inner.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: The original allocation and valid nonzero replacement size come from the caller.
        let replacement = unsafe { self.inner.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            self.counters.change(layout.size(), new_size, 4);
        }
        replacement
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;
    use std::sync::{Arc, Barrier};

    struct RefuseLarge;

    // SAFETY: Small allocations delegate to System unchanged; rejected operations return null
    // without taking ownership of or changing the original allocation.
    unsafe impl GlobalAlloc for RefuseLarge {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if layout.size() > 64 {
                return std::ptr::null_mut();
            }
            // SAFETY: The caller supplied a valid layout.
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            // SAFETY: The caller retains the matching System allocation and layout.
            unsafe { System.dealloc(pointer, layout) }
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            if size > 64 {
                return std::ptr::null_mut();
            }
            // SAFETY: Valid System allocation/layout and nonzero new size from the caller.
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    #[test]
    fn failed_allocations_and_reallocation_leave_ownership_and_counters_unchanged() {
        let meter = Metered::new(RefuseLarge);
        let small = Layout::from_size_align(32, 8).expect("layout");
        let large = Layout::from_size_align(128, 8).expect("layout");
        // SAFETY: Valid layouts, checked allocation, failed realloc keeps the old pointer valid.
        unsafe {
            let pointer = meter.alloc(small);
            assert!(!pointer.is_null());
            *pointer = 41;
            let before = meter.snapshot();
            assert!(meter.alloc(large).is_null());
            assert!(meter.alloc_zeroed(large).is_null());
            assert!(meter.realloc(pointer, small, 128).is_null());
            assert_eq!(meter.snapshot(), before);
            assert_eq!(*pointer, 41);
            meter.dealloc(pointer, small);
        }
        assert_eq!(meter.snapshot().live_bytes, 0);
    }

    #[test]
    fn concurrent_phase_resets_never_erase_live_ownership_or_tear_snapshots() {
        let meter = Metered::new(System);
        let start = Barrier::new(5);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let layout = Layout::from_size_align(256, 8).expect("layout");
                    start.wait();
                    for _ in 0..4000 {
                        // SAFETY: Each thread exclusively owns and frees its checked allocation.
                        unsafe {
                            let pointer = meter.alloc(layout);
                            assert!(!pointer.is_null());
                            std::hint::spin_loop();
                            meter.dealloc(pointer, layout);
                        }
                    }
                });
            }
            start.wait();
            for _ in 0..4000 {
                for state in [meter.begin_phase(), meter.snapshot()] {
                    assert!(state.peak_live_bytes >= state.live_bytes);
                    assert_eq!(
                        state.live_bytes,
                        state.bytes_requested - state.bytes_released
                    );
                    assert_eq!(
                        state.live_bytes,
                        (state.allocations - state.deallocations) * 256
                    );
                }
            }
        });
        let state = meter.snapshot();
        assert_eq!(state.live_bytes, 0);
        assert_eq!(state.allocations, 16000);
        assert_eq!(state.bytes_requested, state.bytes_released);
    }

    #[test]
    fn peak_survives_release_and_phase_reset_keeps_existing_ownership() {
        let meter = Metered::new(System);
        let small = Layout::from_size_align(64, 16).expect("layout");
        let large = Layout::from_size_align(1024, 16).expect("layout");
        // SAFETY: Nonzero valid layouts, checked successful allocations, and matching deallocations.
        unsafe {
            let a = meter.alloc(small);
            let b = meter.alloc_zeroed(large);
            assert!(!a.is_null() && !b.is_null());
            assert_eq!(*b, 0);
            assert_eq!(meter.snapshot().peak_live_bytes, 1088);
            meter.dealloc(b, large);
            assert_eq!(meter.snapshot().live_bytes, 64);
            assert_eq!(meter.snapshot().peak_live_bytes, 1088);
            assert_eq!(meter.begin_phase().peak_live_bytes, 64);
            meter.dealloc(a, small);
        }
        assert_eq!(meter.snapshot().live_bytes, 0);
        assert_eq!(meter.snapshot().allocations, 2);
        assert_eq!(meter.snapshot().deallocations, 2);
    }

    #[test]
    fn realloc_growth_and_shrink_preserve_content_and_exact_live_sizes() {
        let meter = Metered::new(System);
        let layout = Layout::from_size_align(32, 8).expect("layout");
        // SAFETY: Each successful realloc replaces the prior pointer/layout and is freed once.
        unsafe {
            let a = meter.alloc(layout);
            assert!(!a.is_null());
            *a = 79;
            let b = meter.realloc(a, layout, 4096);
            assert!(!b.is_null());
            assert_eq!(*b, 79);
            let grown = Layout::from_size_align(4096, 8).expect("layout");
            let c = meter.realloc(b, grown, 16);
            assert!(!c.is_null());
            assert_eq!(*c, 79);
            assert_eq!(meter.snapshot().live_bytes, 16);
            assert_eq!(meter.snapshot().peak_live_bytes, 4096);
            meter.dealloc(c, Layout::from_size_align(16, 8).expect("layout"));
        }
        let state = meter.snapshot();
        assert_eq!(
            (state.live_bytes, state.allocations, state.reallocations),
            (0, 1, 2)
        );
        assert_eq!(state.bytes_requested, state.bytes_released);
    }

    #[test]
    fn simultaneous_allocations_are_counted_across_threads() {
        let meter = Arc::new(Metered::new(System));
        let held = Arc::new(Barrier::new(9));
        let release = Arc::new(Barrier::new(9));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (meter, held, release) =
                    (Arc::clone(&meter), Arc::clone(&held), Arc::clone(&release));
                scope.spawn(move || {
                    let layout = Layout::from_size_align(2048, 8).expect("layout");
                    // SAFETY: This thread keeps exclusive ownership and frees its checked allocation.
                    unsafe {
                        let pointer = meter.alloc(layout);
                        assert!(!pointer.is_null());
                        held.wait();
                        release.wait();
                        meter.dealloc(pointer, layout);
                    }
                });
            }
            held.wait();
            let snapshot = meter.snapshot();
            // Release threads before asserting so a failure cannot deadlock the test scope.
            release.wait();
            assert_eq!(snapshot.live_bytes, 8 * 2048);
            assert_eq!(snapshot.peak_live_bytes, 8 * 2048);
        });
        assert_eq!(meter.snapshot().live_bytes, 0);
    }
}
