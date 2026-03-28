//! Cascade invariant checks (ADR-007).
//!
//! I-1 runs in ALL builds (production sentinel).
//! I-2..I-7 are debug_assert only.

use crate::graph::wfg::WaitForGraph;

/// I-1: Weight Conservation (production sentinel).
///
/// Checks I-2 (non-amplification) + I-7 (locality). This runs in ALL
/// builds — it is the first line of defense against algorithm bugs.
///
/// Note: global sum equality (`Σ attributed == Σ raw`) does NOT hold
/// after cascade redistribution. The cascade absorbs weight at
/// intermediate nodes, so `total_attributed ≤ total_raw`. Per-edge
/// non-amplification (I-2) is the correct conservation check.
pub fn is_conserved(original: &WaitForGraph, result: &WaitForGraph) -> bool {
    check_non_amplification(result) && check_locality(original, result)
}

/// I-1 enforcement: call after every cascade run.
/// Panics in debug builds, logs warning in release builds.
/// Returns the conservation status.
pub fn assert_weight_conserved(original: &WaitForGraph, result: &WaitForGraph) -> bool {
    let i2 = check_non_amplification(result);
    let i7 = check_locality(original, result);
    let conserved = i2 && i7;

    if !conserved {
        if cfg!(debug_assertions) {
            panic!(
                "I-1 VIOLATION: conservation check failed (I-2={}, I-7={})",
                i2, i7
            );
        } else {
            eprintln!("[wperf] WARNING: I-1 violation (I-2={}, I-7={})", i2, i7);
        }
    }

    conserved
}

/// I-2: Non-amplification.
/// No edge's attributed_delay_ms may exceed its raw_wait_ms.
pub fn check_non_amplification(result: &WaitForGraph) -> bool {
    result
        .all_edges()
        .iter()
        .all(|(_, _, _, ew)| ew.attributed_delay_ms <= ew.raw_wait_ms)
}

/// I-3: Non-negativity.
/// All attributed_delay_ms >= 0. Trivially true for u64, but documents intent.
pub fn check_non_negativity(_result: &WaitForGraph) -> bool {
    true // u64 is always >= 0
}

/// I-4: Termination.
/// Cascade must not create or remove nodes/edges.
/// The topology of the result must match the original.
pub fn check_termination(original: &WaitForGraph, result: &WaitForGraph) -> bool {
    original.node_count() == result.node_count() && original.edge_count() == result.edge_count()
}

/// I-7: Locality.
/// Every edge in result must correspond to an edge in original with
/// the same (src, dst) and time_window.
pub fn check_locality(original: &WaitForGraph, result: &WaitForGraph) -> bool {
    let orig_edges = original.all_edges();
    let res_edges = result.all_edges();

    if orig_edges.len() != res_edges.len() {
        return false;
    }

    for (orig, res) in orig_edges.iter().zip(res_edges.iter()) {
        // Same endpoints
        if orig.1 != res.1 || orig.2 != res.2 {
            return false;
        }
        // Same time window
        if orig.3.time_window != res.3.time_window {
            return false;
        }
        // Same raw_wait
        if orig.3.raw_wait_ms != res.3.raw_wait_ms {
            return false;
        }
    }

    true
}

/// I-5: Idempotency.
/// cascade(cascade(G)) == cascade(G).
/// Test-only — running cascade twice is expensive.
pub fn check_idempotency(graph: &WaitForGraph, max_depth: u32) -> bool {
    use super::engine::cascade_engine;

    let first = cascade_engine(graph, Some(max_depth));
    let second = cascade_engine(&first, Some(max_depth));

    // Compare all attributed_delay_ms values
    let e1 = first.all_edges();
    let e2 = second.all_edges();

    if e1.len() != e2.len() {
        return false;
    }

    e1.iter()
        .zip(e2.iter())
        .all(|(a, b)| a.3.attributed_delay_ms == b.3.attributed_delay_ms)
}

/// I-6: Depth monotonicity.
/// Increasing max_depth propagates more weight downstream, so
/// total_attributed(deep) ≤ total_attributed(shallow).
/// Test-only — runs cascade at two depths.
pub fn check_depth_monotonicity(graph: &WaitForGraph) -> bool {
    use super::engine::cascade_engine;

    let shallow = cascade_engine(graph, Some(2));
    let deep = cascade_engine(graph, Some(10));

    deep.total_attributed() <= shallow.total_attributed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::*;

    #[test]
    fn conservation_passes_on_identity() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let result = g.clone_with_reset_attribution();
        assert!(is_conserved(&g, &result));
    }

    #[test]
    fn conservation_passes_on_cascade_result() {
        use crate::cascade::engine::cascade_engine;
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

        let result = cascade_engine(&g, None);
        assert!(is_conserved(&g, &result));
    }

    #[test]
    fn conservation_detects_amplification() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let mut bad = g.clone_with_reset_attribution();
        // Corrupt: set attributed > raw
        let edges = bad.all_edges();
        let eidx = edges[0].0;
        bad.edge_weight_mut(eidx).attributed_delay_ms = 999;
        assert!(!is_conserved(&g, &bad));
    }

    #[test]
    fn conservation_detects_locality_violation() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        // Add an extra edge in result — violates locality
        let mut bad = g.clone_with_reset_attribution();
        bad.add_node(ThreadId(3), NodeKind::UserThread);
        bad.add_edge(ThreadId(1), ThreadId(3), TimeWindow::new(0, 30));
        assert!(!is_conserved(&g, &bad));
    }

    #[test]
    fn non_amplification_passes() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));
        assert!(check_non_amplification(&g));
    }

    #[test]
    fn non_amplification_detects_violation() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let edges = g.all_edges();
        let eidx = edges[0].0;
        g.edge_weight_mut(eidx).attributed_delay_ms = 999;
        assert!(!check_non_amplification(&g));
    }

    #[test]
    fn non_negativity_trivially_true() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));
        // u64 cannot be negative — this documents the type-level guarantee
        assert!(check_non_negativity(&g));
    }

    #[test]
    fn termination_passes_on_clone() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let result = g.clone_with_reset_attribution();
        assert!(check_termination(&g, &result));
    }

    #[test]
    fn termination_detects_topology_change() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let mut bad = g.clone_with_reset_attribution();
        bad.add_node(ThreadId(3), NodeKind::UserThread);
        bad.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 30));
        assert!(!check_termination(&g, &bad));
    }

    #[test]
    fn idempotency_passes_on_figure4() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

        assert!(check_idempotency(&g, 10));
    }

    #[test]
    fn depth_monotonicity_passes_on_chain() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_node(ThreadId(4), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));
        g.add_edge(ThreadId(3), ThreadId(4), TimeWindow::new(50, 100));

        assert!(check_depth_monotonicity(&g));
    }

    #[test]
    fn locality_passes_on_clone() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let result = g.clone_with_reset_attribution();
        assert!(check_locality(&g, &result));
    }

    #[test]
    fn locality_detects_extra_edge() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let mut bad = g.clone_with_reset_attribution();
        bad.add_node(ThreadId(3), NodeKind::UserThread);
        bad.add_edge(ThreadId(1), ThreadId(3), TimeWindow::new(0, 30));
        assert!(!check_locality(&g, &bad));
    }

    #[test]
    fn assert_weight_conserved_returns_true_on_valid() {
        use crate::cascade::engine::cascade_engine;
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let result = cascade_engine(&g, None);
        assert!(assert_weight_conserved(&g, &result));
    }

    #[test]
    fn termination_detects_node_only_change() {
        // Same edge count but different node count
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let mut bad = g.clone_with_reset_attribution();
        bad.add_node(ThreadId(3), NodeKind::UserThread);
        // Node count differs but edge count is the same
        assert!(!check_termination(&g, &bad));
    }

    #[test]
    fn locality_detects_src_only_change() {
        // Only src endpoint differs, dst is the same
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let mut bad = WaitForGraph::new();
        bad.add_node(ThreadId(3), NodeKind::UserThread);
        bad.add_node(ThreadId(2), NodeKind::UserThread);
        bad.add_edge(ThreadId(3), ThreadId(2), TimeWindow::new(0, 50));
        assert!(!check_locality(&g, &bad));
    }

    #[test]
    fn idempotency_actually_runs_cascade() {
        use crate::cascade::engine::cascade_engine;
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

        // Verify cascade actually changes attributed values
        let result = cascade_engine(&g, Some(10));
        let edges = result.all_edges();
        let e12 = edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
            .unwrap();
        assert_ne!(
            e12.3.attributed_delay_ms, e12.3.raw_wait_ms,
            "cascade must change attribution"
        );

        // Then verify idempotency
        assert!(check_idempotency(&g, 10));
    }

    #[test]
    fn depth_monotonicity_verified_numerically() {
        use crate::cascade::engine::cascade_engine;
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 100));

        let shallow = cascade_engine(&g, Some(2));
        let deep = cascade_engine(&g, Some(10));
        // Deep cascade propagates more → less total attributed
        assert!(deep.total_attributed() <= shallow.total_attributed());
        assert!(check_depth_monotonicity(&g));
    }

    #[test]
    fn locality_detects_window_change() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        // Create result with different time window
        let mut bad = WaitForGraph::new();
        bad.add_node(ThreadId(1), NodeKind::UserThread);
        bad.add_node(ThreadId(2), NodeKind::UserThread);
        bad.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 99));
        assert!(!check_locality(&g, &bad));
    }
}
