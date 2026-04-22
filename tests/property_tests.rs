//! Property-based tests: 10K random graphs, invariants I-2 through I-5, I-7 (ADR-016).
//!
//! Uses proptest to generate random WFGs biased toward realistic
//! characteristics: sparse, mostly acyclic, depth 3-8.

use proptest::prelude::*;

use wperf::cascade::engine::cascade_engine;
use wperf::cascade::invariants::{
    check_idempotency, check_locality, check_non_amplification, check_non_negativity,
    check_termination, invariants_ok,
};
use wperf::correlate::correlate_events;
use wperf::format::event::{EventType, WperfEvent};
use wperf::graph::types::*;
use wperf::graph::wfg::WaitForGraph;
use wperf::scc::tarjan::build_condensation;

/// Generate a random WFG with realistic characteristics.
fn arb_wfg() -> impl Strategy<Value = WaitForGraph> {
    // 2-20 nodes, 1-30 edges
    (2usize..=20, 1usize..=30)
        .prop_flat_map(|(node_count, max_edges)| {
            let edge_count = max_edges.min(node_count * 3); // sparse
            let edges = proptest::collection::vec(
                (
                    0..node_count, // src
                    0..node_count, // dst
                    0u64..1000,    // start_ms
                    1u64..500,     // duration
                ),
                1..=edge_count,
            );
            (Just(node_count), edges)
        })
        .prop_map(|(node_count, edges)| {
            let mut g = WaitForGraph::new();
            for i in 0..node_count {
                let kind = if i == 0 {
                    NodeKind::UserThread
                } else {
                    match i % 5 {
                        0 => NodeKind::KernelThread,
                        1..=3 => NodeKind::UserThread,
                        _ => NodeKind::PseudoDisk,
                    }
                };
                g.add_node(ThreadId(i64::try_from(i).unwrap()), kind);
            }

            for (src, dst, start, dur) in edges {
                if src != dst {
                    // Skip self-loops for simpler graphs
                    let end = start + dur;
                    g.add_edge(
                        ThreadId(i64::try_from(src).unwrap()),
                        ThreadId(i64::try_from(dst).unwrap()),
                        TimeWindow::new(start, end),
                    );
                }
            }
            g
        })
        .prop_filter("graph must have at least 1 edge", |g| g.edge_count() > 0)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn all_invariants_hold(g in arb_wfg()) {
        let result = cascade_engine(&g, None).unwrap();

        // Production sentinel: I-2 ∧ I-7 (ADR-016)
        prop_assert!(invariants_ok(&g, &result), "invariants_ok (I-2 ∧ I-7) failed");

        // I-2: Non-amplification
        prop_assert!(check_non_amplification(&result), "I-2 (non-amplification) failed");

        // I-3: Non-negativity
        prop_assert!(check_non_negativity(&result), "I-3 (non-negativity) failed");

        // I-4: Termination (topology preserved)
        prop_assert!(check_termination(&g, &result), "I-4 (termination) failed");

        // I-7: Locality
        prop_assert!(check_locality(&g, &result), "I-7 (locality) failed");
    }

    #[test]
    fn idempotency_holds(g in arb_wfg()) {
        // I-5: Separate test — runs cascade twice, slower
        prop_assert!(check_idempotency(&g, 10), "I-5 (idempotency) failed");
    }

    // I-6 (depth monotonicity) removed from property testing.
    // It only holds for simple chains — fan-out and concurrent waiters
    // cause child_absorbed < window.duration(), breaking monotonicity.
    // See invariants.rs I-6 doc. Unit tests cover the chain case.

    #[test]
    fn condensation_is_acyclic(g in arb_wfg()) {
        let result = cascade_engine(&g, None).unwrap();
        let cdag = build_condensation(&result);
        // Condensation must be a DAG — toposort should succeed
        prop_assert!(
            petgraph::algo::toposort(&cdag.dag, None).is_ok(),
            "condensation DAG has a cycle"
        );
    }
}

// =============================================================================
// Phase 2b #38 commit-4 — Synthetic IO edge invariants
// =============================================================================
//
// Property: graphs produced by correlate_events() from random IoIssue /
// IoComplete event streams satisfy `invariants_ok` (I-2 ∧ I-7) after cascade.
// ADR-009 specifies pseudo-threads participate fully in Tarjan analysis; this
// proptest guards against regressions where synthetic edge generation drifts
// away from the correlate → cascade contract.

#[allow(clippy::similar_names)] // tgid / tid mirror WperfEvent field names
fn io_event(ts_ns: u64, tgid: u32, tid: u32, dev: u32, sector: u64, kind: EventType) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts_ns,
        pid: tgid,
        tid,
        // io_sector() packs prev_tid | next_tid<<32
        prev_tid: u32::try_from(sector & 0xFFFF_FFFF).unwrap(),
        next_tid: u32::try_from(sector >> 32).unwrap(),
        // io_dev() reads prev_pid, io_nr_sector() reads next_pid
        prev_pid: dev,
        next_pid: 8,
        cpu: 0,
        event_type: kind as u8,
        prev_state: 0,
        flags: 0,
    }
}

/// Generate a random sorted stream of `IoIssue` + (optional) `IoComplete` events.
///
/// Each I/O is characterized by `(issuer_tid, dev, sector, issue_ts, delta_ns,
/// pairs)`. When `pairs` is true an `IoComplete` event follows at `issue_ts +
/// delta_ns`; when false the issue dangles (pending-at-end — orphan counter
/// scaffolding, commit-5).
fn arb_io_stream() -> impl Strategy<Value = Vec<WperfEvent>> {
    let io_spec = (
        1u32..=8,                   // tid  (1..=8 keeps graphs small + diverse)
        0x800_0001u32..=0x800_0008, // dev
        0u64..=0xFFFF,              // sector
        0u64..=10_000_000,          // issue_ts_ns (0..10ms range)
        1_000u64..=5_000_000,       // delta_ns (1μs..5ms service time)
        any::<bool>(),              // paired?
    );

    prop::collection::vec(io_spec, 1..=50)
        .prop_map(|specs| {
            let mut events: Vec<WperfEvent> = Vec::with_capacity(specs.len() * 2);
            let mut seen_keys: std::collections::HashSet<(u32, u64)> =
                std::collections::HashSet::new();

            for (tid, dev, sector, issue_ts, delta, paired) in specs {
                // Dedupe (dev, sector) within this shrink — the userspace
                // IoKey = (dev, sector, nr_sector), and all nr_sector are fixed
                // at 8 here so same (dev, sector) would collide on overwrite.
                // The overwrite behavior is validated elsewhere; the proptest
                // focuses on graph invariants, not last-writer-wins semantics.
                if !seen_keys.insert((dev, sector)) {
                    continue;
                }
                events.push(io_event(
                    issue_ts,
                    tid,
                    tid,
                    dev,
                    sector,
                    EventType::IoIssue,
                ));
                if paired {
                    let complete_ts = issue_ts.saturating_add(delta);
                    events.push(io_event(
                        complete_ts,
                        tid,
                        tid,
                        dev,
                        sector,
                        EventType::IoComplete,
                    ));
                }
            }

            events.sort_by_key(|e| e.timestamp_ns);
            events
        })
        .prop_filter("stream must produce at least one IoIssue", |v| {
            !v.is_empty()
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn io_synthetic_edges_preserve_invariants_ok(events in arb_io_stream()) {
        let (graph, _) = correlate_events(&events, 0);
        if graph.edge_count() == 0 {
            // Edge-less graphs are trivially invariant-preserving and the
            // cascade engine asserts non-empty input.
            return Ok(());
        }

        let result = cascade_engine(&graph, None).expect("cascade must not fail on valid WFG");

        prop_assert!(
            invariants_ok(&graph, &result),
            "I-2 ∧ I-7 production sentinel failed on synthetic-edge graph"
        );
        prop_assert!(check_non_amplification(&result), "I-2 violated");
        prop_assert!(check_non_negativity(&result), "I-3 violated");
        prop_assert!(check_termination(&graph, &result), "I-4 violated");
        prop_assert!(check_locality(&graph, &result), "I-7 violated");
    }

    #[test]
    fn io_synthetic_edges_idempotent(events in arb_io_stream()) {
        let (graph, _) = correlate_events(&events, 0);
        if graph.edge_count() == 0 {
            return Ok(());
        }
        prop_assert!(check_idempotency(&graph, 3), "I-5 (idempotency) failed");
    }
}
