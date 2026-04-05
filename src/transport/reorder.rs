//! Min-Heap reorder buffer for perfarray transport.
//!
//! Per-CPU perfarray buffers deliver events in per-CPU timestamp order,
//! but there is no global ordering guarantee across CPUs. This module
//! provides a bounded-latency reorder buffer that restores approximate
//! global timestamp ordering.
//!
//! # Algorithm
//!
//! Events are inserted into a binary min-heap keyed by `timestamp_ns`.
//! When a new event arrives, any buffered events whose timestamps are
//! older than `newest_timestamp - window_ns` are flushed in order.
//! This provides a bounded reordering window (default 50ms) that handles
//! typical cross-CPU clock skew on modern hardware.
//!
//! # Perfarray-only
//!
//! Ring buffer transport delivers events in global order and does not
//! need this buffer. The collector should only use `ReorderBuf` when
//! the transport mode is `PerfArray`.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::format::event::WperfEvent;

/// Default reorder window: 50 milliseconds in nanoseconds.
const DEFAULT_WINDOW_NS: u64 = 50_000_000;

/// A min-heap reorder buffer that restores global timestamp order
/// from per-CPU perfarray event streams.
///
/// Events are buffered until they fall outside the reorder window
/// relative to the newest event seen, at which point they are flushed
/// in timestamp order via the provided callback.
#[derive(Debug)]
pub struct ReorderBuf {
    /// Min-heap of events, ordered by timestamp (smallest first via `Reverse`).
    heap: BinaryHeap<Reverse<WperfEvent>>,
    /// The largest timestamp seen so far.
    newest_ts: u64,
    /// Reorder window in nanoseconds.
    window_ns: u64,
}

impl ReorderBuf {
    /// Create a new reorder buffer with the default 50ms window.
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW_NS)
    }

    /// Create a new reorder buffer with a custom window in nanoseconds.
    pub fn with_window(window_ns: u64) -> Self {
        Self {
            heap: BinaryHeap::new(),
            newest_ts: 0,
            window_ns,
        }
    }

    /// Insert an event and flush any events that have aged out of the
    /// reorder window.
    ///
    /// Returns the number of events flushed to the callback.
    pub fn push(&mut self, event: WperfEvent, callback: &mut dyn FnMut(&WperfEvent)) -> usize {
        if event.timestamp_ns > self.newest_ts {
            self.newest_ts = event.timestamp_ns;
        }
        self.heap.push(Reverse(event));
        self.flush_expired(callback)
    }

    /// Flush all events that have aged out of the reorder window.
    ///
    /// An event is considered expired when:
    /// `newest_ts - event.timestamp_ns >= window_ns`
    fn flush_expired(&mut self, callback: &mut dyn FnMut(&WperfEvent)) -> usize {
        let cutoff = self.newest_ts.saturating_sub(self.window_ns);
        let mut flushed = 0;
        while let Some(Reverse(oldest)) = self.heap.peek() {
            if oldest.timestamp_ns <= cutoff {
                let Reverse(event) = self.heap.pop().unwrap();
                callback(&event);
                flushed += 1;
            } else {
                break;
            }
        }
        flushed
    }

    /// Drain all remaining buffered events in timestamp order.
    ///
    /// Call this at the end of a recording session to ensure no events
    /// are left in the reorder buffer.
    pub fn drain(&mut self, callback: &mut dyn FnMut(&WperfEvent)) -> usize {
        let mut drained = 0;
        while let Some(Reverse(event)) = self.heap.pop() {
            callback(&event);
            drained += 1;
        }
        self.newest_ts = 0;
        drained
    }

    /// Number of events currently buffered.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

impl Default for ReorderBuf {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::event::WperfEvent;

    fn make_event(ts: u64) -> WperfEvent {
        WperfEvent {
            timestamp_ns: ts,
            pid: 0,
            tid: 0,
            prev_tid: 0,
            next_tid: 0,
            prev_pid: 0,
            next_pid: 0,
            cpu: 0,
            event_type: 1,
            prev_state: 0,
            flags: 0,
        }
    }

    #[test]
    fn empty_buffer() {
        let buf = ReorderBuf::new();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn events_within_window_are_buffered() {
        let mut buf = ReorderBuf::with_window(100);
        let mut out = vec![];
        buf.push(make_event(10), &mut |e| out.push(e.timestamp_ns));
        buf.push(make_event(50), &mut |e| out.push(e.timestamp_ns));

        // Both events are within 100ns window of newest (50), so nothing flushed.
        assert!(out.is_empty());
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn events_outside_window_are_flushed_in_order() {
        let mut buf = ReorderBuf::with_window(100);
        let mut out = vec![];

        buf.push(make_event(10), &mut |e| out.push(e.timestamp_ns));
        buf.push(make_event(50), &mut |e| out.push(e.timestamp_ns));
        // Push event at ts=200; cutoff = 200 - 100 = 100.
        // Events at ts=10 and ts=50 are both <= 100, so both flushed.
        buf.push(make_event(200), &mut |e| out.push(e.timestamp_ns));

        assert_eq!(out, vec![10, 50]);
        assert_eq!(buf.len(), 1); // ts=200 still buffered
    }

    #[test]
    fn out_of_order_events_are_sorted() {
        let mut buf = ReorderBuf::with_window(50);
        let mut out = vec![];

        // Insert out of CPU order: cpu0=100, cpu1=80, cpu2=90
        buf.push(make_event(100), &mut |e| out.push(e.timestamp_ns));
        buf.push(make_event(80), &mut |e| out.push(e.timestamp_ns));
        buf.push(make_event(90), &mut |e| out.push(e.timestamp_ns));

        // Nothing flushed yet (all within 50ns of newest=100).
        assert!(out.is_empty());

        // Push event at ts=200; cutoff = 200 - 50 = 150.
        // All three (80, 90, 100) are <= 150, flushed in sorted order.
        buf.push(make_event(200), &mut |e| out.push(e.timestamp_ns));

        assert_eq!(out, vec![80, 90, 100]);
    }

    #[test]
    fn drain_flushes_all_in_order() {
        let mut buf = ReorderBuf::with_window(1_000_000);
        let mut out = vec![];

        buf.push(make_event(300), &mut |_| {});
        buf.push(make_event(100), &mut |_| {});
        buf.push(make_event(200), &mut |_| {});

        let drained = buf.drain(&mut |e| out.push(e.timestamp_ns));
        assert_eq!(drained, 3);
        assert_eq!(out, vec![100, 200, 300]);
        assert!(buf.is_empty());
    }

    #[test]
    fn default_window_is_50ms() {
        let buf = ReorderBuf::default();
        assert_eq!(buf.window_ns, 50_000_000);
    }

    #[test]
    fn zero_window_flushes_immediately() {
        let mut buf = ReorderBuf::with_window(0);
        let mut out = vec![];

        buf.push(make_event(10), &mut |e| out.push(e.timestamp_ns));
        // With window=0, cutoff = 10 - 0 = 10, so event at ts=10 is flushed.
        assert_eq!(out, vec![10]);
        assert!(buf.is_empty());
    }

    #[test]
    fn push_returns_flush_count() {
        let mut buf = ReorderBuf::with_window(50);

        let count = buf.push(make_event(10), &mut |_| {});
        assert_eq!(count, 0);

        let count = buf.push(make_event(100), &mut |_| {});
        assert_eq!(count, 1); // ts=10 flushed (100 - 50 = 50 cutoff, 10 <= 50)
    }

    #[test]
    fn duplicate_timestamps_handled() {
        let mut buf = ReorderBuf::with_window(10);
        let mut out = vec![];

        buf.push(make_event(100), &mut |_| {});
        buf.push(make_event(100), &mut |_| {});
        buf.push(make_event(100), &mut |_| {});

        let drained = buf.drain(&mut |e| out.push(e.timestamp_ns));
        assert_eq!(drained, 3);
        assert_eq!(out, vec![100, 100, 100]);
    }

    #[test]
    fn monotonically_increasing_events_flush_progressively() {
        let mut buf = ReorderBuf::with_window(100);
        let mut out = vec![];

        for ts in (0..10).map(|i| i * 50) {
            buf.push(make_event(ts), &mut |e| out.push(e.timestamp_ns));
        }

        // Events flushed as each new event pushes older ones past the window.
        // Final drain to get remaining.
        buf.drain(&mut |e| out.push(e.timestamp_ns));

        // All events should appear in sorted order.
        let sorted: Vec<u64> = (0..10).map(|i| i * 50).collect();
        assert_eq!(out, sorted);
    }
}
