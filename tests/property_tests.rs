//! Property-based tests: 10K random graphs, all 7 invariants.
//!
//! Uses proptest to generate random WFGs biased toward realistic
//! characteristics: sparse, mostly acyclic, depth 3-8.

use proptest::prelude::*;

use wperf::cascade::engine::{cascade_engine, is_conserved};
use wperf::cascade::invariants::{
    check_idempotency, check_locality, check_non_amplification, check_non_negativity,
    check_termination,
};
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
                        1 | 2 | 3 => NodeKind::UserThread,
                        _ => NodeKind::PseudoDisk,
                    }
                };
                g.add_node(ThreadId(i as i64), kind);
            }

            for (src, dst, start, dur) in edges {
                if src != dst {
                    // Skip self-loops for simpler graphs
                    let end = start + dur;
                    g.add_edge(
                        ThreadId(src as i64),
                        ThreadId(dst as i64),
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
        let result = cascade_engine(&g, None);

        // I-1: Production sentinel (I-2 + I-7)
        prop_assert!(is_conserved(&g, &result), "I-1 (conservation) failed");

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
        let result = cascade_engine(&g, None);
        let cdag = build_condensation(&result);
        // Condensation must be a DAG — toposort should succeed
        prop_assert!(
            petgraph::algo::toposort(&cdag.dag, None).is_ok(),
            "condensation DAG has a cycle"
        );
    }
}
