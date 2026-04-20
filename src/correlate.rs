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

use crate::format::event::{EventType, WperfEvent, futex_op};
use crate::graph::types::{NodeKind, ThreadId, TimeWindow, WaitType};
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
    /// Futex wait type, set if a `FutexWait` event preceded the switch-out.
    /// `None` = no futex event preceded this switch-out.
    wait_type: Option<WaitType>,
}

/// Convert a futex op value to a `WaitType`.
fn futex_op_to_wait_type(op: u32) -> WaitType {
    match op {
        futex_op::FUTEX_WAIT => WaitType::FutexWait,
        futex_op::FUTEX_LOCK_PI => WaitType::FutexLockPi,
        futex_op::FUTEX_WAIT_BITSET => WaitType::FutexWaitBitset,
        futex_op::FUTEX_WAIT_REQUEUE_PI => WaitType::FutexWaitRequeuePi,
        _ => WaitType::Unknown,
    }
}

/// Maximum gap between `sys_enter_futex` and `sched_switch` for the futex
/// to be considered the cause of the sleep. The kernel path from
/// `do_futex` → `futex_wait_setup` → `schedule()` is typically < 100μs;
/// 1ms provides 10x margin for high-load scheduling delays.
const FUTEX_CORRELATION_WINDOW_NS: u64 = 1_000_000;

/// Default spurious wakeup filter threshold: 50μs in nanoseconds (§2.5).
/// Threads that run for less than this after being woken are classified as
/// spurious wakeups and their edges excluded from the graph.
pub const DEFAULT_SPURIOUS_THRESHOLD_NS: u64 = 50_000;

/// Pending futex event for a thread (before it goes off-CPU).
#[derive(Debug, Clone)]
struct PendingFutex {
    timestamp_ns: u64,
    wait_type: WaitType,
}

/// A deferred edge awaiting spurious wakeup check (§2.5).
/// Created at switch-in, committed or discarded at next switch-out.
#[derive(Debug, Clone)]
struct PendingEdge {
    switch_in_ns: u64,
    src: ThreadId,
    dst: ThreadId,
    window: TimeWindow,
    wait_type: Option<WaitType>,
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
    /// Edges filtered as spurious wakeups (on-CPU < threshold after wakeup).
    /// **Canonical coverage metric** (§2.5 / §3.8).
    pub false_wakeup_filtered_count: u64,
}

/// Correlate a globally timestamp-sorted event stream into a `WaitForGraph`.
///
/// `spurious_threshold_ns` controls the spurious wakeup filter (§2.5):
/// edges where the woken thread runs for less than this duration (in
/// nanoseconds) before sleeping again are discarded as noise. Pass `0`
/// to disable filtering.
///
/// Returns the populated graph and correlation statistics.
///
/// # Panics
///
/// Debug builds assert that the input is sorted by `timestamp_ns`.
pub fn correlate_events(
    events: &[WperfEvent],
    spurious_threshold_ns: u64,
) -> (WaitForGraph, CorrelationStats) {
    debug_assert!(
        events
            .windows(2)
            .all(|w| w[0].timestamp_ns <= w[1].timestamp_ns),
        "correlate_events: input events must be sorted by timestamp_ns"
    );

    let mut graph = WaitForGraph::new();
    let mut stats = CorrelationStats::default();
    let mut off_cpu: HashMap<u32, OffCpuRecord> = HashMap::new();
    let mut pending_futex: HashMap<u32, PendingFutex> = HashMap::new();
    let mut pending_edges: HashMap<u32, PendingEdge> = HashMap::new();

    for event in events {
        stats.events_processed += 1;

        match EventType::from_u8(event.event_type) {
            Some(EventType::Switch) => {
                handle_switch(
                    event,
                    &mut graph,
                    &mut off_cpu,
                    &mut pending_futex,
                    &mut pending_edges,
                    &mut stats,
                    spurious_threshold_ns,
                );
            }
            Some(EventType::Wakeup | EventType::WakeupNew) => {
                handle_wakeup(event, &mut off_cpu, &mut stats);
            }
            Some(EventType::Exit) => {
                commit_pending_edge(&mut pending_edges, event.tid, &mut graph, &mut stats);
                off_cpu.remove(&event.tid);
                pending_futex.remove(&event.tid);
            }
            Some(EventType::FutexWait) => {
                handle_futex_wait(event, &mut pending_futex);
            }
            Some(EventType::IoIssue) | Some(EventType::IoComplete) => {
                // IO synth-edge generation lands in a later commit (issue #38
                // commit-4 per plan). Scaffold: enum variants wired end-to-end
                // so BPF discriminants stay in lockstep; no graph mutation yet.
            }
            None => {}
        }
    }

    for (_, pe) in pending_edges.drain() {
        add_edge_from_pending(&mut graph, pe, &mut stats);
    }

    (graph, stats)
}

/// Commit a `PendingEdge` into the graph unconditionally.
#[allow(clippy::needless_pass_by_value)]
fn add_edge_from_pending(graph: &mut WaitForGraph, pe: PendingEdge, stats: &mut CorrelationStats) {
    graph.add_node(pe.src, NodeKind::UserThread);
    graph.add_node(pe.dst, NodeKind::UserThread);
    if let Some(wt) = pe.wait_type {
        graph.add_edge_with_wait_type(pe.src, pe.dst, pe.window, wt);
    } else {
        graph.add_edge(pe.src, pe.dst, pe.window);
    }
    stats.edges_created += 1;
}

/// If `tid` has a pending edge, commit it to the graph.
fn commit_pending_edge(
    pending_edges: &mut HashMap<u32, PendingEdge>,
    tid: u32,
    graph: &mut WaitForGraph,
    stats: &mut CorrelationStats,
) {
    if let Some(pe) = pending_edges.remove(&tid) {
        add_edge_from_pending(graph, pe, stats);
    }
}

/// Handle a `sched_switch` event.
///
/// Three things happen in a single switch:
/// 1. Resolve any pending edge for `prev_tid` (spurious wakeup check)
/// 2. `prev_tid` is being switched **out** (may go off-CPU)
/// 3. `next_tid` is being switched **in** (may create a deferred edge)
fn handle_switch(
    event: &WperfEvent,
    graph: &mut WaitForGraph,
    off_cpu: &mut HashMap<u32, OffCpuRecord>,
    pending_futex: &mut HashMap<u32, PendingFutex>,
    pending_edges: &mut HashMap<u32, PendingEdge>,
    stats: &mut CorrelationStats,
    spurious_threshold_ns: u64,
) {
    // --- Resolve pending edge for prev_tid (§2.5 spurious wakeup check) ---
    #[allow(clippy::collapsible_if)]
    if event.prev_tid != 0 {
        if let Some(pe) = pending_edges.remove(&event.prev_tid) {
            let on_cpu_ns = event.timestamp_ns.saturating_sub(pe.switch_in_ns);
            if event.prev_state != TASK_RUNNING && on_cpu_ns < spurious_threshold_ns {
                stats.false_wakeup_filtered_count += 1;
            } else {
                add_edge_from_pending(graph, pe, stats);
            }
        }
    }

    // --- prev_tid goes off-CPU (if not preempted) ---
    if event.prev_state != TASK_RUNNING && event.prev_tid != 0 {
        let wait_type = pending_futex.remove(&event.prev_tid).and_then(|pf| {
            if event.timestamp_ns.saturating_sub(pf.timestamp_ns) <= FUTEX_CORRELATION_WINDOW_NS {
                Some(pf.wait_type)
            } else {
                None
            }
        });

        off_cpu.insert(
            event.prev_tid,
            OffCpuRecord {
                switch_out_ns: event.timestamp_ns,
                waker_tid: None,
                wait_type,
            },
        );
    }

    // --- next_tid comes on-CPU (defer edge if possible) ---
    if event.next_tid == 0 {
        return;
    }

    if let Some(record) = off_cpu.remove(&event.next_tid) {
        if let Some(waker_tid) = record.waker_tid {
            let wait_duration_ns = event.timestamp_ns.saturating_sub(record.switch_out_ns);

            let src = ThreadId(i64::from(event.next_tid));
            let dst = ThreadId(i64::from(waker_tid));

            let start_ms = record.switch_out_ns / 1_000_000;
            let window = TimeWindow::new(start_ms, start_ms + wait_duration_ns / 1_000_000);

            pending_edges.insert(
                event.next_tid,
                PendingEdge {
                    switch_in_ns: event.timestamp_ns,
                    src,
                    dst,
                    window,
                    wait_type: record.wait_type,
                },
            );
        } else {
            stats.switch_in_without_waker_count += 1;
        }
    } else {
        stats.unmatched_switch_in_count += 1;
    }
}

/// Handle a `FutexWait` event — record pending futex for the calling thread.
fn handle_futex_wait(event: &WperfEvent, pending_futex: &mut HashMap<u32, PendingFutex>) {
    pending_futex.insert(
        event.tid,
        PendingFutex {
            timestamp_ns: event.timestamp_ns,
            wait_type: futex_op_to_wait_type(event.futex_op()),
        },
    );
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

    fn correlate(events: &[WperfEvent]) -> (WaitForGraph, CorrelationStats) {
        correlate_events(events, DEFAULT_SPURIOUS_THRESHOLD_NS)
    }

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
        let (graph, stats) = correlate(&[]);
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

        let (graph, stats) = correlate(&events);

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

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.edges_created, 0);
        assert_eq!(stats.unmatched_switch_in_count, 2); // both next_tids have no off-CPU record
        assert_eq!(graph.node_count(), 0);
    }

    #[test]
    fn unmatched_wakeup() {
        // Wakeup for a thread not in off-CPU table.
        let events = vec![wakeup_event(1_000_000, 200, 100)];

        let (_, stats) = correlate(&events);

        assert_eq!(stats.unmatched_wakeup_count, 1);
        assert_eq!(stats.edges_created, 0);
    }

    #[test]
    fn switch_in_without_prior_switch_out() {
        // Thread appears on-CPU without prior switch-out (trace start).
        let events = vec![switch_event(1_000_000, 200, 100, 0)];

        let (_, stats) = correlate(&events);

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

        let (graph, stats) = correlate(&events);

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

        let (_, stats) = correlate(&events);

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

        let (_, stats) = correlate(&events);

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

        let (graph, stats) = correlate(&events);

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

        let (graph, stats) = correlate(&events);

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

        let (graph, stats) = correlate(&events);

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

        let (graph, stats) = correlate(&events);

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

        let (_, stats) = correlate(&events);

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
        assert_eq!(stats.false_wakeup_filtered_count, 0);
    }

    #[test]
    fn time_window_precision() {
        let events = vec![
            switch_event(1_500_000, 100, 200, 1),
            wakeup_event(5_800_000, 200, 100),
            switch_event(10_200_000, 200, 100, 0),
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.edges_created, 1);
        let edges = graph.all_edges();
        let ew = edges[0].3;
        assert_eq!(ew.raw_wait_ms, 8);
        assert_eq!(ew.time_window.start_ms, 1);
        assert_eq!(ew.time_window.end_ms, 9);
    }

    fn futex_event(ts: u64, tid: u32, op: u32) -> WperfEvent {
        WperfEvent {
            timestamp_ns: ts,
            pid: 0,
            tid,
            prev_tid: 0,
            next_tid: 0,
            prev_pid: 0,
            next_pid: 0,
            cpu: 0,
            event_type: EventType::FutexWait as u8,
            prev_state: 0,
            flags: op,
        }
    }

    #[test]
    fn futex_wait_annotates_edge() {
        let events = vec![
            futex_event(900_000, 100, futex_op::FUTEX_WAIT),
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.edges_created, 1);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, Some(WaitType::FutexWait));
    }

    #[test]
    fn futex_lock_pi_annotates_edge() {
        let events = vec![
            futex_event(900_000, 100, futex_op::FUTEX_LOCK_PI),
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];

        let (graph, _) = correlate(&events);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, Some(WaitType::FutexLockPi));
    }

    #[test]
    fn futex_wait_bitset_annotates_edge() {
        let events = vec![
            futex_event(900_000, 100, futex_op::FUTEX_WAIT_BITSET),
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];

        let (graph, _) = correlate(&events);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, Some(WaitType::FutexWaitBitset));
    }

    #[test]
    fn futex_wait_requeue_pi_annotates_edge() {
        let events = vec![
            futex_event(900_000, 100, futex_op::FUTEX_WAIT_REQUEUE_PI),
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];

        let (graph, _) = correlate(&events);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, Some(WaitType::FutexWaitRequeuePi));
    }

    #[test]
    fn no_futex_event_gives_unknown_wait_type() {
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];

        let (graph, _) = correlate(&events);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, None);
    }

    #[test]
    fn futex_consumed_on_switch_out() {
        // Futex event for T100, T100 goes off-CPU (consuming it),
        // then T100 comes back and goes off again without futex → Unknown.
        let events = vec![
            futex_event(900_000, 100, futex_op::FUTEX_WAIT),
            switch_event(1_000_000, 100, 200, 1), // consumes futex
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0), // T100 back
            switch_event(4_000_000, 100, 200, 1), // T100 off again, no futex
            wakeup_event(5_000_000, 200, 100),
            switch_event(6_000_000, 200, 100, 0),
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.edges_created, 2);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, Some(WaitType::FutexWait));
        assert_eq!(edges[1].3.wait_type, None);
    }

    #[test]
    fn futex_cleared_on_exit() {
        let events = vec![
            futex_event(900_000, 100, futex_op::FUTEX_WAIT),
            exit_event(1_000_000, 100),
        ];

        let (_, stats) = correlate(&events);
        assert_eq!(stats.edges_created, 0);
    }

    #[test]
    fn futex_for_different_thread_no_cross_contamination() {
        // Futex for T100, but T200 goes off-CPU → T200's edge should be Unknown.
        let events = vec![
            futex_event(900_000, 100, futex_op::FUTEX_LOCK_PI),
            switch_event(1_000_000, 200, 300, 1), // T200 off (not T100)
            wakeup_event(2_000_000, 300, 200),
            switch_event(3_000_000, 300, 200, 0),
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.edges_created, 1);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, None);
    }

    #[test]
    fn stale_futex_discarded_outside_window() {
        // Futex returns -EAGAIN (no block), thread later blocks on IO.
        // The futex event is >1ms before the switch → stale, discarded.
        let events = vec![
            futex_event(1_000_000, 100, futex_op::FUTEX_WAIT),
            // >1ms gap (futex returned -EAGAIN, thread did other work)
            switch_event(5_000_000, 100, 200, 1), // T100 off (IO block)
            wakeup_event(6_000_000, 200, 100),
            switch_event(7_000_000, 200, 100, 0),
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.edges_created, 1);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, None); // stale futex discarded
    }

    #[test]
    fn futex_within_window_accepted() {
        // Futex event <1ms before switch → valid correlation.
        let events = vec![
            futex_event(999_500, 100, futex_op::FUTEX_WAIT),
            switch_event(1_000_000, 100, 200, 1), // 500ns later
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];

        let (graph, _) = correlate(&events);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, Some(WaitType::FutexWait));
    }

    #[test]
    fn futex_at_window_boundary_accepted() {
        // Exactly at 1ms boundary → accepted (<=).
        let events = vec![
            futex_event(0, 100, futex_op::FUTEX_LOCK_PI),
            switch_event(1_000_000, 100, 200, 1), // exactly 1ms
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];

        let (graph, _) = correlate(&events);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, Some(WaitType::FutexLockPi));
    }

    // --- Spurious wakeup filter tests (§2.5) ---

    #[test]
    fn spurious_wakeup_filtered() {
        // Thread wakes, runs for 20μs (< 50μs threshold), sleeps again → spurious.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1), // T100 off-CPU
            wakeup_event(2_000_000, 200, 100),    // T200 wakes T100
            switch_event(3_000_000, 200, 100, 0), // T100 on-CPU
            switch_event(3_020_000, 100, 200, 1), // T100 off after 20μs → spurious
            wakeup_event(5_000_000, 200, 100),    // T200 wakes T100 again
            switch_event(6_000_000, 200, 100, 0), // T100 on-CPU (real this time)
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.false_wakeup_filtered_count, 1);
        assert_eq!(stats.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn spurious_wakeup_at_threshold_kept() {
        // Thread runs for exactly 50μs (= threshold) → NOT spurious (< is strict).
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0), // T100 on-CPU
            switch_event(3_050_000, 100, 200, 1), // T100 off after exactly 50μs
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.false_wakeup_filtered_count, 0);
        assert_eq!(stats.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn spurious_wakeup_above_threshold_kept() {
        // Thread runs for 100μs (> 50μs threshold) → kept.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
            switch_event(3_100_000, 100, 200, 1), // 100μs on-CPU
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.false_wakeup_filtered_count, 0);
        assert_eq!(stats.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn spurious_preempted_always_committed() {
        // Thread runs for 10μs but is preempted (RUNNING) → committed, not spurious.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0), // T100 on-CPU
            switch_event(3_010_000, 100, 200, 0), // preempted after 10μs (RUNNING)
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.false_wakeup_filtered_count, 0);
        assert_eq!(stats.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn spurious_edge_not_committed_at_trace_end() {
        // Thread wakes and never switches out → committed at end-of-trace.
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.false_wakeup_filtered_count, 0);
        assert_eq!(stats.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn spurious_exit_commits_edge() {
        // Thread wakes, runs briefly, then exits → committed (meaningful work).
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
            exit_event(3_010_000, 100), // exits after 10μs
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.false_wakeup_filtered_count, 0);
        assert_eq!(stats.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn spurious_threshold_zero_disables_filter() {
        // With threshold=0, even 0μs on-CPU is kept (0 < 0 is false).
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
            switch_event(3_000_000, 100, 200, 1), // 0ns on-CPU
        ];

        let (graph, stats) = correlate_events(&events, 0);

        assert_eq!(stats.false_wakeup_filtered_count, 0);
        assert_eq!(stats.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn spurious_condvar_pattern() {
        // Classic condvar spurious: futex_wait → wake → check (< 50μs) → futex_wait again.
        // First edge filtered, second edge (with new futex annotation) committed.
        let events = vec![
            futex_event(900_000, 100, futex_op::FUTEX_WAIT),
            switch_event(1_000_000, 100, 200, 1), // T100 off (futex)
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0), // T100 on-CPU
            // T100 checks predicate, not satisfied, goes back to futex_wait
            futex_event(3_010_000, 100, futex_op::FUTEX_WAIT),
            switch_event(3_020_000, 100, 200, 1), // T100 off after 20μs → spurious
            wakeup_event(5_000_000, 200, 100),
            switch_event(6_000_000, 200, 100, 0), // real wakeup
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.false_wakeup_filtered_count, 1);
        assert_eq!(stats.edges_created, 1);
        let edges = graph.all_edges();
        assert_eq!(edges[0].3.wait_type, Some(WaitType::FutexWait));
    }

    #[test]
    fn spurious_multiple_threads_independent() {
        // Two threads both have spurious wakeups — independent tracking.
        let events = vec![
            switch_event(1_000_000, 100, 300, 1), // T100 off
            switch_event(1_500_000, 200, 300, 1), // T200 off
            wakeup_event(2_000_000, 300, 100),    // T300 wakes T100
            wakeup_event(2_500_000, 300, 200),    // T300 wakes T200
            switch_event(3_000_000, 300, 100, 0), // T100 on (T300 preempted)
            switch_event(3_010_000, 100, 300, 1), // T100 off after 10μs → spurious
            switch_event(3_500_000, 300, 200, 0), // T200 on (T300 preempted)
            switch_event(3_510_000, 200, 300, 1), // T200 off after 10μs → spurious
        ];

        let (graph, stats) = correlate(&events);

        assert_eq!(stats.false_wakeup_filtered_count, 2);
        assert_eq!(stats.edges_created, 0);
        assert_eq!(graph.edge_count(), 0);
    }
}
