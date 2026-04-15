//! Event correlation — the **correlate sub-step** of `final-design.md §3.2`.
//!
//! Converts raw scheduling events into Wait-For-Graph edges using a
//! three-event pattern:
//! 1. `sched_switch` (`prev_state` != RUNNING) → thread goes off-CPU
//! 2. `sched_wakeup` → record waker as cause of off-CPU thread
//! 3. `sched_switch` (`next_tid` matches off-CPU thread) → finalize wait edge
//!
//! This module implements the correlate sub-step plus minimal
//! orphan-accounting needed for `unmatched_wakeup_count`, but not the
//! full surrounding parse/reorder pipeline. Parse and Reorder are
//! upstream pipeline stages.
//!
//! # Input contract
//!
//! The input event slice **must be globally sorted by `timestamp_ns`**.
//! Sorting is the caller's responsibility (via reorder buffer or file-level
//! sort). Unsorted input is a caller error and will produce incorrect edges.
//!
//! # Multi-wakeup policy
//!
//! If a thread receives multiple wakeups while off-CPU, **last-wake-wins**:
//! the most recent waker overwrites any prior waker. This matches kernel
//! semantics where only the final successful `try_to_wake_up()` before
//! schedule-in is causally relevant.

use std::collections::HashMap;

use serde::Serialize;

use crate::format::event::{EventType, WperfEvent};
use crate::graph::types::{NodeKind, ThreadId, TimeWindow};
use crate::graph::wfg::WaitForGraph;

/// Linux `TASK_RUNNING` state value. A `sched_switch` with `prev_state == 0`
/// means the thread was preempted (still runnable), not voluntarily sleeping.
const TASK_RUNNING: u8 = 0;

/// Tracks a thread that has gone off-CPU.
#[derive(Debug, Clone)]
struct OffCpuRecord {
    /// Timestamp when the thread was switched out.
    switch_out_ns: u64,
    /// Waker tid, set by a subsequent `sched_wakeup` event (last-wake-wins).
    waker_tid: Option<u32>,
}

/// Statistics from the correlation pass.
///
/// # Canonical vs diagnostic metrics
///
/// - `unmatched_wakeup_count` is the **canonical coverage metric** defined in
///   `final-design.md §3.2 / §3.8`. It measures correlation completeness.
/// - All other counters are **internal diagnostic stats** for debugging and
///   are not part of the exported observability contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CorrelationStats {
    /// Total events processed.
    pub events_processed: u64,
    /// Wait-For edges created in the graph.
    pub edges_created: u64,
    /// Wakeup events where the target was not in the off-CPU table.
    /// **Canonical coverage metric** (§3.2 / §3.8).
    pub unmatched_wakeup_count: u64,
    /// Switch-in events where the thread had no prior switch-out record.
    /// *Internal diagnostic stat* — not a canonical coverage metric.
    pub unmatched_switch_in_count: u64,
    /// Switch-in events where off-CPU record had no waker (no wakeup seen).
    /// *Internal diagnostic stat* — not a canonical coverage metric.
    pub switch_in_without_waker_count: u64,
}

/// Correlate a globally timestamp-sorted event stream into a `WaitForGraph`.
///
/// Returns the populated graph and correlation statistics.
///
/// # Panics
///
/// Debug builds assert that the input is sorted by `timestamp_ns`.
pub fn correlate_events(events: &[WperfEvent]) -> (WaitForGraph, CorrelationStats) {
    debug_assert!(
        events
            .windows(2)
            .all(|w| w[0].timestamp_ns <= w[1].timestamp_ns),
        "correlate_events: input events must be sorted by timestamp_ns"
    );

    let mut graph = WaitForGraph::new();
    let mut stats = CorrelationStats::default();
    let mut off_cpu: HashMap<u32, OffCpuRecord> = HashMap::new();

    for event in events {
        stats.events_processed += 1;

        match EventType::from_u8(event.event_type) {
            Some(EventType::Switch) => {
                handle_switch(event, &mut graph, &mut off_cpu, &mut stats);
            }
            Some(EventType::Wakeup | EventType::WakeupNew) => {
                handle_wakeup(event, &mut off_cpu, &mut stats);
            }
            Some(EventType::Exit) => {
                // Clean up off-CPU record if thread exits.
                off_cpu.remove(&event.tid);
            }
            Some(EventType::FutexWait) | None => {
                // FutexWait: consumed by wait_type annotation (task #35).
                // None: unknown event type — skip (forward-compat).
            }
        }
    }

    (graph, stats)
}

/// Handle a `sched_switch` event.
///
/// Two things happen in a single switch:
/// - `prev_tid` is being switched **out** (may go off-CPU)
/// - `next_tid` is being switched **in** (may finalize a wait edge)
fn handle_switch(
    event: &WperfEvent,
    graph: &mut WaitForGraph,
    off_cpu: &mut HashMap<u32, OffCpuRecord>,
    stats: &mut CorrelationStats,
) {
    // --- prev_tid goes off-CPU (if not preempted) ---
    if event.prev_state != TASK_RUNNING && event.prev_tid != 0 {
        off_cpu.insert(
            event.prev_tid,
            OffCpuRecord {
                switch_out_ns: event.timestamp_ns,
                waker_tid: None,
            },
        );
    }

    // --- next_tid comes on-CPU (finalize edge if possible) ---
    if event.next_tid == 0 {
        return; // idle thread, skip
    }

    if let Some(record) = off_cpu.remove(&event.next_tid) {
        if let Some(waker_tid) = record.waker_tid {
            // We have a complete causal chain: switch-out → wakeup → switch-in.
            // Use floor(delta_ns / 1e6) for duration to preserve raw_wait_ms
            // accuracy. Independent endpoint truncation would inflate wait
            // times (e.g. 1.2ms → 2ms). See team review discussion on #92.
            let off_cpu_ms = event.timestamp_ns.saturating_sub(record.switch_out_ns) / 1_000_000;

            // Ensure both nodes exist.
            let src = ThreadId(i64::from(event.next_tid));
            let dst = ThreadId(i64::from(waker_tid));
            graph.add_node(src, NodeKind::UserThread);
            graph.add_node(dst, NodeKind::UserThread);

            let start_ms = record.switch_out_ns / 1_000_000;
            graph.add_edge(src, dst, TimeWindow::new(start_ms, start_ms + off_cpu_ms));

            stats.edges_created += 1;
        } else {
            // Thread was off-CPU but no wakeup was observed.
            // This can happen with preemption reschedule or timer wakeups
            // that don't go through sched_wakeup tracepoint.
            stats.switch_in_without_waker_count += 1;
        }
    } else {
        // Switch-in without a prior switch-out record.
        // Normal at trace start (thread was already off-CPU before recording).
        stats.unmatched_switch_in_count += 1;
    }
}

/// Handle a `sched_wakeup` or `sched_wakeup_new` event.
///
/// Records the waker for an off-CPU thread. Last-wake-wins policy:
/// if the target already has a recorded waker, it is overwritten.
fn handle_wakeup(
    event: &WperfEvent,
    off_cpu: &mut HashMap<u32, OffCpuRecord>,
    stats: &mut CorrelationStats,
) {
    // next_tid = target (wakee), prev_tid = source (waker) per BPF event contract
    let target = event.next_tid;
    let source = event.prev_tid;

    // Skip idle thread as waker — kernel context, not a real causal source.
    if source == 0 {
        return;
    }

    if let Some(record) = off_cpu.get_mut(&target) {
        // Last-wake-wins: overwrite any previous waker.
        record.waker_tid = Some(source);
    } else {
        // Wakee is not in off-CPU table — spurious or out-of-window wakeup.
        stats.unmatched_wakeup_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::event::EventType;

    fn switch_event(ts: u64, prev_tid: u32, next_tid: u32, prev_state: u8) -> WperfEvent {
        WperfEvent {
            timestamp_ns: ts,
            pid: 0,
            tid: 0,
            prev_tid,
            next_tid,
            prev_pid: 0,
            next_pid: 0,
            cpu: 0,
            event_type: EventType::Switch as u8,
            prev_state,
            flags: 0,
        }
    }

    fn wakeup_event(ts: u64, source: u32, target: u32) -> WperfEvent {
        WperfEvent {
            timestamp_ns: ts,
            pid: 0,
            tid: 0,
            prev_tid: source,
            next_tid: target,
            prev_pid: 0,
            next_pid: 0,
            cpu: 0,
            event_type: EventType::Wakeup as u8,
            prev_state: 0,
            flags: 0,
        }
    }

    fn exit_event(ts: u64, tid: u32) -> WperfEvent {
        WperfEvent {
            timestamp_ns: ts,
            pid: 0,
            tid,
            prev_tid: 0,
            next_tid: 0,
            prev_pid: 0,
            next_pid: 0,
            cpu: 0,
            event_type: EventType::Exit as u8,
            prev_state: 0,
            flags: 0,
        }
    }

    #[test]
    fn empty_input() {
        let (graph, stats) = correlate_events(&[]);
        assert_eq!(graph.node_count(), 0);
        assert_eq!(stats.events_processed, 0);
        assert_eq!(stats.edges_created, 0);
    }

    #[test]
    fn simple_switch_wakeup_switch() {
        // Thread 100 goes off-CPU, thread 200 wakes it, thread 100 comes back.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1), // T100 off-CPU (INTERRUPTIBLE)
            wakeup_event(2_000_000, 200, 100),    // T200 wakes T100
            switch_event(3_000_000, 200, 100, 0), // T100 comes on-CPU
        ];

        let (graph, stats) = correlate_events(&events);

        assert_eq!(stats.events_processed, 3);
        assert_eq!(stats.edges_created, 1);
        assert_eq!(stats.unmatched_wakeup_count, 0);

        // Edge: T100 (waiter) → T200 (waitee)
        let edges = graph.all_edges();
        assert_eq!(edges.len(), 1);
        let (_, src_tid, dst_tid, ew) = &edges[0];
        assert_eq!(*src_tid, ThreadId(100));
        assert_eq!(*dst_tid, ThreadId(200));
        assert_eq!(ew.raw_wait_ms, 2); // 3M - 1M = 2M ns = 2ms
    }

    #[test]
    fn preempted_thread_no_edge() {
        // Thread 100 is preempted (prev_state=0=RUNNING), no off-CPU record.
        let events = vec![
            switch_event(1_000_000, 100, 200, 0), // T100 preempted
            switch_event(2_000_000, 200, 100, 0), // T100 back
        ];

        let (graph, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 0);
        assert_eq!(stats.unmatched_switch_in_count, 2); // both next_tids have no off-CPU record
        assert_eq!(graph.node_count(), 0);
    }

    #[test]
    fn unmatched_wakeup() {
        // Wakeup for a thread not in off-CPU table.
        let events = vec![wakeup_event(1_000_000, 200, 100)];

        let (_, stats) = correlate_events(&events);

        assert_eq!(stats.unmatched_wakeup_count, 1);
        assert_eq!(stats.edges_created, 0);
    }

    #[test]
    fn switch_in_without_prior_switch_out() {
        // Thread appears on-CPU without prior switch-out (trace start).
        let events = vec![switch_event(1_000_000, 200, 100, 0)];

        let (_, stats) = correlate_events(&events);

        assert_eq!(stats.unmatched_switch_in_count, 1);
    }

    #[test]
    fn multi_wakeup_last_wins() {
        // Thread 100 off-CPU, T200 wakes it, then T300 wakes it again.
        // Last-wake-wins: edge should point to T300.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1), // T100 off-CPU
            wakeup_event(2_000_000, 200, 100),    // T200 wakes T100
            wakeup_event(3_000_000, 300, 100),    // T300 wakes T100 (overwrites)
            switch_event(4_000_000, 300, 100, 0), // T100 comes on-CPU
        ];

        let (graph, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 1);
        let edges = graph.all_edges();
        let (_, src_tid, dst_tid, ew) = &edges[0];
        assert_eq!(*src_tid, ThreadId(100));
        assert_eq!(*dst_tid, ThreadId(300)); // last waker wins
        assert_eq!(ew.raw_wait_ms, 3); // 4M - 1M = 3ms
    }

    #[test]
    fn switch_in_without_waker() {
        // Thread goes off-CPU but is switched back in without a wakeup event.
        // This can happen with timer-based reschedule.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1), // T100 off-CPU
            switch_event(3_000_000, 200, 100, 0), // T100 on-CPU, no wakeup
        ];

        let (_, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 0);
        assert_eq!(stats.switch_in_without_waker_count, 1);
    }

    #[test]
    fn exit_cleans_up_off_cpu() {
        // Thread goes off-CPU then exits — no edge, no dangling state.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1), // T100 off-CPU
            exit_event(2_000_000, 100),           // T100 exits
        ];

        let (_, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 0);
        assert_eq!(stats.events_processed, 2);
    }

    #[test]
    fn wakeup_new_same_as_wakeup() {
        // WakeupNew should be handled identically to Wakeup.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1), // T100 off-CPU
            WperfEvent {
                timestamp_ns: 2_000_000,
                pid: 0,
                tid: 0,
                prev_tid: 200, // waker
                next_tid: 100, // wakee
                prev_pid: 0,
                next_pid: 0,
                cpu: 0,
                event_type: EventType::WakeupNew as u8,
                prev_state: 0,
                flags: 0,
            },
            switch_event(3_000_000, 200, 100, 0), // T100 on-CPU
        ];

        let (graph, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 1);
        let edges = graph.all_edges();
        assert_eq!(edges[0].2, ThreadId(200));
    }

    #[test]
    fn multiple_threads_concurrent() {
        // Two threads go off-CPU independently, both get woken up.
        let events = vec![
            switch_event(1_000_000, 100, 300, 1), // T100 off-CPU
            switch_event(1_500_000, 200, 300, 2), // T200 off-CPU (UNINTERRUPTIBLE)
            wakeup_event(2_000_000, 300, 100),    // T300 wakes T100
            wakeup_event(2_500_000, 400, 200),    // T400 wakes T200
            switch_event(3_000_000, 300, 100, 0), // T100 on-CPU
            switch_event(3_500_000, 300, 200, 0), // T200 on-CPU
        ];

        let (graph, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 2);
        assert_eq!(graph.node_count(), 4); // T100, T200, T300, T400
    }

    #[test]
    fn idle_thread_zero_ignored() {
        // tid 0 (idle/swapper) should not create off-CPU records or edges.
        let events = vec![
            switch_event(1_000_000, 0, 100, 1), // idle goes off (ignored)
            switch_event(2_000_000, 100, 0, 0), // T100 switches to idle (next=0 ignored)
        ];

        let (graph, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 0);
        assert_eq!(graph.node_count(), 0);
    }

    #[test]
    fn wakeup_from_idle_ignored() {
        // Wakeup with source=0 (idle/kernel context) should not record a waker.
        // The thread should switch-in without a waker, not create an edge to tid 0.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1), // T100 off-CPU
            wakeup_event(2_000_000, 0, 100),      // idle "wakes" T100 (ignored)
            switch_event(3_000_000, 200, 100, 0), // T100 on-CPU
        ];

        let (graph, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 0);
        assert_eq!(stats.switch_in_without_waker_count, 1);
        assert_eq!(graph.node_count(), 0); // no tid 0 node
    }

    #[test]
    fn unknown_event_type_skipped() {
        let events = vec![WperfEvent {
            timestamp_ns: 1_000_000,
            pid: 0,
            tid: 0,
            prev_tid: 0,
            next_tid: 0,
            prev_pid: 0,
            next_pid: 0,
            cpu: 0,
            event_type: 255, // unknown
            prev_state: 0,
            flags: 0,
        }];

        let (_, stats) = correlate_events(&events);

        assert_eq!(stats.events_processed, 1);
        assert_eq!(stats.edges_created, 0);
    }

    #[test]
    fn stats_default() {
        let stats = CorrelationStats::default();
        assert_eq!(stats.events_processed, 0);
        assert_eq!(stats.edges_created, 0);
        assert_eq!(stats.unmatched_wakeup_count, 0);
        assert_eq!(stats.unmatched_switch_in_count, 0);
        assert_eq!(stats.switch_in_without_waker_count, 0);
    }

    #[test]
    fn time_window_precision() {
        // Verify nanosecond → millisecond conversion and edge weight.
        let events = vec![
            switch_event(1_500_000, 100, 200, 1),  // T100 off at 1.5ms
            wakeup_event(5_800_000, 200, 100),     // T200 wakes T100
            switch_event(10_200_000, 200, 100, 0), // T100 on at 10.2ms
        ];

        let (graph, stats) = correlate_events(&events);

        assert_eq!(stats.edges_created, 1);
        let edges = graph.all_edges();
        let ew = edges[0].3;
        // 10_200_000 - 1_500_000 = 8_700_000 ns → floor(8.7ms) = 8ms
        assert_eq!(ew.raw_wait_ms, 8);
        assert_eq!(ew.time_window.start_ms, 1); // 1_500_000 / 1M = 1
        assert_eq!(ew.time_window.end_ms, 9); // 1 + 8 = 9
    }
}
