use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::atomic::{try_update_u64, try_update_usize};

use crate::Completion;

/// Current or historical pressure on the Engine's bounded resources.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ResourceMetrics {
    inflight_requests: usize,
    queued_commands: usize,
    queued_callbacks: usize,
    reserved_stream_queue_bytes: usize,
    active_connections: usize,
    idle_connections: usize,
    connection_waiters: usize,
}

impl ResourceMetrics {
    /// Returns requests that are accepted but not fully released.
    ///
    /// A terminal callback remains inflight until its callback job returns.
    pub fn inflight_requests(&self) -> usize {
        self.inflight_requests
    }

    /// Returns commands currently waiting for the Engine owner.
    pub fn queued_commands(&self) -> usize {
        self.queued_commands
    }

    /// Returns terminal callback jobs waiting for a callback worker or manual dispatch.
    pub fn queued_callbacks(&self) -> usize {
        self.queued_callbacks
    }

    /// Returns bytes reserved against the Engine-wide streaming queue budget.
    pub fn reserved_stream_queue_bytes(&self) -> usize {
        self.reserved_stream_queue_bytes
    }

    /// Returns native connection-capacity slots, including DNS, connecting, leased, and idle.
    pub fn active_connections(&self) -> usize {
        self.active_connections
    }

    /// Returns native idle connections, or zero when connection metrics are unavailable.
    pub fn idle_connections(&self) -> usize {
        self.idle_connections
    }

    /// Returns native connection-capacity waiters, or zero when connection metrics are unavailable.
    pub fn connection_waiters(&self) -> usize {
        self.connection_waiters
    }
}

/// A payload-free, approximate snapshot of one Engine's lifetime activity and pressure.
///
/// Counters saturate rather than wrap and cannot be reset. Each field is loaded independently, so
/// a snapshot taken while work is progressing may be cross-field inconsistent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct EngineMetrics {
    connection_metrics_available: bool,
    requests_accepted: u64,
    requests_completed: u64,
    requests_failed: u64,
    requests_cancelled: u64,
    connections_opened: u64,
    connections_reused: u64,
    connections_closed: u64,
    idle_connections_evicted: u64,
    current: ResourceMetrics,
    high_water: ResourceMetrics,
}

impl EngineMetrics {
    /// Reports whether this Engine owns and measures physical connection/pool lifecycles.
    ///
    /// When false, connection counters and connection-specific gauges remain zero because the
    /// selected backend does not expose trustworthy physical connection events. Request and queue
    /// metrics remain available.
    pub fn connection_metrics_available(&self) -> bool {
        self.connection_metrics_available
    }

    /// Returns the number of requests accepted during this Engine's lifetime.
    pub fn requests_accepted(&self) -> u64 {
        self.requests_accepted
    }

    /// Returns accepted requests whose canonical terminal outcome was a response.
    pub fn requests_completed(&self) -> u64 {
        self.requests_completed
    }

    /// Returns accepted requests whose canonical terminal outcome was failure.
    pub fn requests_failed(&self) -> u64 {
        self.requests_failed
    }

    /// Returns accepted requests whose canonical terminal outcome was cancellation.
    pub fn requests_cancelled(&self) -> u64 {
        self.requests_cancelled
    }

    /// Returns native connection-capacity lifecycles begun.
    ///
    /// A lifecycle starts when DNS/connect capacity is reserved, before a socket necessarily
    /// reaches the connected state, and ends when that reservation is released.
    pub fn connections_opened(&self) -> u64 {
        self.connections_opened
    }

    /// Returns native connection-pool reuses, or zero when connection metrics are unavailable.
    pub fn connections_reused(&self) -> u64 {
        self.connections_reused
    }

    /// Returns native connection-capacity lifecycles released.
    pub fn connections_closed(&self) -> u64 {
        self.connections_closed
    }

    /// Returns native idle evictions, or zero when connection metrics are unavailable.
    pub fn idle_connections_evicted(&self) -> u64 {
        self.idle_connections_evicted
    }

    /// Returns the current bounded-resource snapshot.
    pub fn current(&self) -> ResourceMetrics {
        self.current
    }

    /// Returns lifetime high-water marks for bounded resources.
    pub fn high_water(&self) -> ResourceMetrics {
        self.high_water
    }
}

#[derive(Default)]
pub(crate) struct Metrics {
    connection_metrics_available: AtomicBool,
    requests_accepted: AtomicU64,
    requests_completed: AtomicU64,
    requests_failed: AtomicU64,
    requests_cancelled: AtomicU64,
    connections_opened: AtomicU64,
    connections_reused: AtomicU64,
    connections_closed: AtomicU64,
    idle_connections_evicted: AtomicU64,
    queued_commands: AtomicUsize,
    queued_callbacks: AtomicUsize,
    active_connections: AtomicUsize,
    idle_connections: AtomicUsize,
    connection_waiters: AtomicUsize,
    high_inflight_requests: AtomicUsize,
    high_queued_commands: AtomicUsize,
    high_queued_callbacks: AtomicUsize,
    high_reserved_stream_queue_bytes: AtomicUsize,
    high_active_connections: AtomicUsize,
    high_idle_connections: AtomicUsize,
    high_connection_waiters: AtomicUsize,
}

impl Metrics {
    pub(crate) fn enable_connection_metrics(&self) {
        self.connection_metrics_available
            .store(true, Ordering::Release);
    }

    pub(crate) fn request_accepted(&self, inflight: usize, stream_bytes: usize) {
        saturating_increment(&self.requests_accepted);
        update_max(&self.high_inflight_requests, inflight);
        update_max(&self.high_reserved_stream_queue_bytes, stream_bytes);
    }

    pub(crate) fn request_terminal(&self, completion: &Completion) {
        match completion {
            Completion::Completed(_) => saturating_increment(&self.requests_completed),
            Completion::Failed(_) => saturating_increment(&self.requests_failed),
            Completion::Cancelled => saturating_increment(&self.requests_cancelled),
        }
    }

    pub(crate) fn stream_terminal(&self, outcome: crate::stream::StreamOutcome) {
        match outcome {
            crate::stream::StreamOutcome::Completed => {
                saturating_increment(&self.requests_completed)
            }
            crate::stream::StreamOutcome::Failed => saturating_increment(&self.requests_failed),
            crate::stream::StreamOutcome::Cancelled => {
                saturating_increment(&self.requests_cancelled)
            }
        }
    }

    pub(crate) fn command_queued(&self) {
        increment_gauge(&self.queued_commands, &self.high_queued_commands);
    }

    pub(crate) fn commands_drained(&self, count: usize) {
        subtract_gauge(&self.queued_commands, count);
    }

    pub(crate) fn callback_queued(&self) {
        increment_gauge(&self.queued_callbacks, &self.high_queued_callbacks);
    }

    pub(crate) fn callback_dequeued(&self) {
        subtract_gauge(&self.queued_callbacks, 1);
    }

    #[cfg(feature = "native")]
    pub(crate) fn connection_opened(&self, active: usize) {
        saturating_increment(&self.connections_opened);
        self.set_active_connections(active);
    }

    #[cfg(feature = "native")]
    pub(crate) fn connection_reused(&self) {
        saturating_increment(&self.connections_reused);
    }

    #[cfg(feature = "native")]
    pub(crate) fn connection_closed(&self, active: usize) {
        saturating_increment(&self.connections_closed);
        self.set_active_connections(active);
    }

    #[cfg(feature = "native")]
    pub(crate) fn idle_evicted(&self) {
        saturating_increment(&self.idle_connections_evicted);
    }

    #[cfg(feature = "native")]
    pub(crate) fn set_active_connections(&self, value: usize) {
        self.active_connections.store(value, Ordering::Release);
        update_max(&self.high_active_connections, value);
    }

    #[cfg(feature = "native")]
    pub(crate) fn set_idle_connections(&self, value: usize) {
        self.idle_connections.store(value, Ordering::Release);
        update_max(&self.high_idle_connections, value);
    }

    #[cfg(feature = "native")]
    pub(crate) fn set_connection_waiters(&self, value: usize) {
        self.connection_waiters.store(value, Ordering::Release);
        update_max(&self.high_connection_waiters, value);
    }

    pub(crate) fn snapshot(&self, inflight: usize, stream_bytes: usize) -> EngineMetrics {
        EngineMetrics {
            connection_metrics_available: self.connection_metrics_available.load(Ordering::Acquire),
            requests_accepted: self.requests_accepted.load(Ordering::Acquire),
            requests_completed: self.requests_completed.load(Ordering::Acquire),
            requests_failed: self.requests_failed.load(Ordering::Acquire),
            requests_cancelled: self.requests_cancelled.load(Ordering::Acquire),
            connections_opened: self.connections_opened.load(Ordering::Acquire),
            connections_reused: self.connections_reused.load(Ordering::Acquire),
            connections_closed: self.connections_closed.load(Ordering::Acquire),
            idle_connections_evicted: self.idle_connections_evicted.load(Ordering::Acquire),
            current: ResourceMetrics {
                inflight_requests: inflight,
                queued_commands: self.queued_commands.load(Ordering::Acquire),
                queued_callbacks: self.queued_callbacks.load(Ordering::Acquire),
                reserved_stream_queue_bytes: stream_bytes,
                active_connections: self.active_connections.load(Ordering::Acquire),
                idle_connections: self.idle_connections.load(Ordering::Acquire),
                connection_waiters: self.connection_waiters.load(Ordering::Acquire),
            },
            high_water: ResourceMetrics {
                inflight_requests: self.high_inflight_requests.load(Ordering::Acquire),
                queued_commands: self.high_queued_commands.load(Ordering::Acquire),
                queued_callbacks: self.high_queued_callbacks.load(Ordering::Acquire),
                reserved_stream_queue_bytes: self
                    .high_reserved_stream_queue_bytes
                    .load(Ordering::Acquire),
                active_connections: self.high_active_connections.load(Ordering::Acquire),
                idle_connections: self.high_idle_connections.load(Ordering::Acquire),
                connection_waiters: self.high_connection_waiters.load(Ordering::Acquire),
            },
        }
    }
}

fn saturating_increment(counter: &AtomicU64) {
    let _ = try_update_u64(counter, Ordering::AcqRel, Ordering::Acquire, |value| {
        Some(value.saturating_add(1))
    });
}

fn increment_gauge(gauge: &AtomicUsize, high: &AtomicUsize) {
    let previous = try_update_usize(gauge, Ordering::AcqRel, Ordering::Acquire, |value| {
        Some(value.saturating_add(1))
    })
    .unwrap_or_else(|value| value);
    let value = previous.saturating_add(1);
    update_max(high, value);
}

fn subtract_gauge(gauge: &AtomicUsize, amount: usize) {
    let _ = try_update_usize(gauge, Ordering::AcqRel, Ordering::Acquire, |value| {
        Some(value.saturating_sub(amount))
    });
}

fn update_max(high: &AtomicUsize, value: usize) {
    let _ = try_update_usize(high, Ordering::AcqRel, Ordering::Acquire, |current| {
        (value > current).then_some(value)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_and_gauges_saturate_instead_of_wrapping() {
        let metrics = Metrics::default();
        metrics.requests_accepted.store(u64::MAX, Ordering::Release);
        metrics.queued_commands.store(usize::MAX, Ordering::Release);

        metrics.request_accepted(usize::MAX, usize::MAX);
        metrics.command_queued();

        let snapshot = metrics.snapshot(usize::MAX, usize::MAX);
        assert_eq!(snapshot.requests_accepted(), u64::MAX);
        assert_eq!(snapshot.current().queued_commands(), usize::MAX);
        assert_eq!(snapshot.high_water().queued_commands(), usize::MAX);
        assert_eq!(snapshot.high_water().inflight_requests(), usize::MAX);
        assert_eq!(
            snapshot.high_water().reserved_stream_queue_bytes(),
            usize::MAX
        );
    }
}
