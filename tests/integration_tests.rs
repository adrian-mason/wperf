//! Fixture-driven integration tests (W3 #20).
//!
//! Constructs complete `.wperf` event streams via `WperfWriter`, reads them
//! back through `WperfReader`, runs the full pipeline via `build_report()`,
//! and asserts semantic properties of the report output.
//!
//! These are NOT snapshots — they verify structural/semantic invariants of
//! the end-to-end pipeline (parser → sort → correlate → cascade → SCC →
//! critical path → report).

use std::io::Cursor;

use wperf::format::event::{EventType, WperfEvent};
use wperf::format::reader::WperfReader;
use wperf::format::writer::WperfWriter;
use wperf::graph::types::ThreadId;
use wperf::report;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn write_trace(events: &[WperfEvent], drop_count: u64) -> Vec<u8> {
    let buf = Cursor::new(Vec::new());
    let mut w = WperfWriter::new(buf).unwrap();
    for ev in events {
        w.write_event(ev).unwrap();
    }
    let buf = w.finish(drop_count).unwrap();
    buf.into_inner()
}

fn build_report_from(events: &[WperfEvent], drop_count: u64) -> report::ReportOutput {
    let data = write_trace(events, drop_count);
    let mut reader = WperfReader::open(Cursor::new(data)).unwrap();
    report::build_report(&mut reader).unwrap()
}

/// Context switch: `prev_tid` goes off-CPU (`prev_state`=1 means sleeping),
/// `next_tid` comes on-CPU.
fn switch(ts_ns: u64, prev_tid: u32, next_tid: u32) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts_ns,
        pid: 0,
        tid: 0,
        prev_tid,
        next_tid,
        prev_pid: 0,
        next_pid: 0,
        cpu: 0,
        event_type: EventType::Switch as u8,
        prev_state: 1, // sleeping — required for valid off-CPU switch
        flags: 0,
    }
}

/// Wakeup: source wakes target.
fn wakeup(ts_ns: u64, source: u32, target: u32) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts_ns,
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

// ---------------------------------------------------------------------------
// Fixture: empty trace
// ---------------------------------------------------------------------------

#[test]
fn fixture_empty_trace() {
    let report = build_report_from(&[], 0);

    // Empty graph — no edges, no critical path, no knots.
    assert_eq!(report.cascade.edges.len(), 0);
    assert_eq!(report.cascade.graph_metrics.edge_count, 0);
    assert_eq!(report.cascade.graph_metrics.node_count, 0);
    assert!(report.critical_path.is_none());
    assert!(report.knots.is_empty());

    // Pipeline stats reflect zero events.
    assert_eq!(report.stats.events_read, 0);
    assert_eq!(report.stats.correlation.edges_created, 0);
    assert_eq!(report.stats.correlation.events_processed, 0);

    // Health: invariants trivially ok, no drops, no unmatched.
    assert!(report.health.invariants_ok);
    assert_eq!(report.health.drop_count, Some(0));
    assert_eq!(report.health.unmatched_wakeup_count, 0);

    // Unavailable metrics are None.
    assert!(report.health.partial_stack_count.is_none());
    assert!(report.health.cascade_depth_truncation_count.is_none());
    assert!(report.health.false_wakeup_filtered_count.is_none());
}

// ---------------------------------------------------------------------------
// Fixture: single wait edge
// ---------------------------------------------------------------------------

#[test]
fn fixture_single_wait_edge() {
    // T10 goes off-CPU at 1ms, T20 wakes T10 at 2ms, T10 back on at 3ms.
    // Expected: one wait edge T10 → T20, raw_wait = 2ms.
    let events = vec![
        switch(1_000_000, 10, 20),
        wakeup(2_000_000, 20, 10),
        switch(3_000_000, 20, 10),
    ];
    let report = build_report_from(&events, 42);

    // One edge in the cascade.
    assert_eq!(report.cascade.edges.len(), 1);
    assert_eq!(report.cascade.graph_metrics.edge_count, 1);
    assert_eq!(report.cascade.graph_metrics.node_count, 2);

    let edge = &report.cascade.edges[0];
    assert_eq!(edge.src, ThreadId(10));
    assert_eq!(edge.dst, ThreadId(20));
    assert_eq!(edge.raw_wait_ms, 2); // 3ms - 1ms
    assert_eq!(edge.attributed_delay_ms, edge.raw_wait_ms);

    // Critical path exists for non-empty graph.
    let cp = report
        .critical_path
        .as_ref()
        .expect("critical path should exist");
    assert!(!cp.chain.is_empty());
    assert!(cp.total_weight > 0);

    // No knots in a 2-node graph (no cycles).
    assert!(report.knots.is_empty());

    // Pipeline stats.
    assert_eq!(report.stats.events_read, 3);
    assert_eq!(report.stats.correlation.edges_created, 1);
    assert_eq!(report.stats.correlation.unmatched_wakeup_count, 0);

    // Health.
    assert!(report.health.invariants_ok);
    assert_eq!(report.health.drop_count, Some(42));
    assert_eq!(report.health.unmatched_wakeup_count, 0);
}

// ---------------------------------------------------------------------------
// Fixture: multi-hop chain (A → B → C)
// ---------------------------------------------------------------------------

#[test]
fn fixture_multi_hop_chain() {
    // T10 waits on T20, T20 waits on T30 — two sequential wait edges.
    //
    // Timeline:
    //   1ms: T10 off-CPU (→ T20 runs)
    //   2ms: T20 off-CPU (→ T30 runs)
    //   3ms: T30 wakes T20
    //   4ms: T20 back on-CPU
    //   5ms: T20 wakes T10
    //   6ms: T10 back on-CPU
    let events = vec![
        switch(1_000_000, 10, 20), // T10 off
        switch(2_000_000, 20, 30), // T20 off
        wakeup(3_000_000, 30, 20), // T30 wakes T20
        switch(4_000_000, 30, 20), // T20 back on
        wakeup(5_000_000, 20, 10), // T20 wakes T10
        switch(6_000_000, 20, 10), // T10 back on
    ];
    let report = build_report_from(&events, 0);

    // Two edges: T10→T20 and T20→T30.
    assert_eq!(report.cascade.edges.len(), 2);
    assert_eq!(report.cascade.graph_metrics.edge_count, 2);

    // Both edges present with correct weights (order may vary, so find by content).
    let edge_10_20 = report
        .cascade
        .edges
        .iter()
        .find(|e| e.src == ThreadId(10) && e.dst == ThreadId(20))
        .expect("expected edge T10 → T20");
    assert_eq!(edge_10_20.raw_wait_ms, 5); // 6ms - 1ms

    let edge_20_30 = report
        .cascade
        .edges
        .iter()
        .find(|e| e.src == ThreadId(20) && e.dst == ThreadId(30))
        .expect("expected edge T20 → T30");
    assert_eq!(edge_20_30.raw_wait_ms, 2); // 4ms - 2ms

    // Critical path spans a chain of nodes.
    let cp = report
        .critical_path
        .as_ref()
        .expect("critical path should exist");
    assert_eq!(
        cp.chain.len(),
        3,
        "linear 3-node chain should have exactly 3 path nodes"
    );
    assert!(cp.total_weight > 0);

    // 3 nodes in the graph.
    assert_eq!(report.cascade.graph_metrics.node_count, 3);

    // No knots (DAG, no cycles).
    assert!(report.knots.is_empty());

    // Invariants hold.
    assert!(report.health.invariants_ok);
    assert_eq!(report.health.unmatched_wakeup_count, 0);

    // Conservation: total attributed ≤ total raw (cascade redistribution).
    assert!(
        report.cascade.graph_metrics.total_attributed_delay_ms
            <= report.cascade.graph_metrics.total_raw_wait_ms
    );
}

// ---------------------------------------------------------------------------
// Fixture: unmatched events
// ---------------------------------------------------------------------------

#[test]
fn fixture_unmatched_events() {
    // Orphan wakeup with no matching off-CPU switch.
    // Plus a normal matched pair to verify mixed handling.
    let events = vec![
        // Orphan wakeup: T99 wakes T88, but T88 was never switched off.
        wakeup(1_000_000, 99, 88),
        // Normal matched pair: T10 off, T20 wakes T10, T10 back.
        switch(2_000_000, 10, 20),
        wakeup(3_000_000, 20, 10),
        switch(4_000_000, 20, 10),
    ];
    let report = build_report_from(&events, 0);

    // The matched pair produces one edge.
    assert_eq!(report.cascade.edges.len(), 1);
    let edge = &report.cascade.edges[0];
    assert_eq!(edge.src, ThreadId(10));
    assert_eq!(edge.dst, ThreadId(20));

    // Health: 1 unmatched wakeup (the orphan).
    assert_eq!(report.health.unmatched_wakeup_count, 1);

    // Correlation diagnostic: events_processed covers all 4 events.
    assert_eq!(report.stats.events_read, 4);

    // Invariants still hold — unmatched wakeups don't break the graph.
    assert!(report.health.invariants_ok);
}

// ---------------------------------------------------------------------------
// Fixture: drop count propagation
// ---------------------------------------------------------------------------

#[test]
fn fixture_drop_count_propagation() {
    // Verify that BPF-side drop counts are faithfully propagated
    // through the full pipeline into health metrics.
    let report = build_report_from(&[], 12345);
    assert_eq!(report.health.drop_count, Some(12345));
}
