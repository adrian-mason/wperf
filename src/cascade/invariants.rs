//! Cascade invariant checks (ADR-007).
//!
//! I-1 (per-entry-edge conservation) runs in ALL builds via
//! `verify_conservation`, which returns `Result`.
//! I-2..I-7 are debug_assert only.

use std::fmt;

use crate::graph::wfg::WaitForGraph;

/// Error returned when cascade invariant checks fail.
#[derive(Debug)]
pub struct ConservationError {
    pub i2_ok: bool,
    pub i7_ok: bool,
}

impl fmt::Display for ConservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "I-1 VIOLATION: conservation check failed (I-2={}, I-7={})",
            self.i2_ok, self.i7_ok
        )
    }
}

impl std::error::Error for ConservationError {}

/// I-1: Per-entry-edge conservation (production sentinel).
///
/// Checks non-amplification only: no edge's attributed_delay_ms
/// exceeds its raw_wait_ms. This is the per-entry-edge invariant
/// defined in ADR-015.
pub fn is_conserved(result: &WaitForGraph) -> bool {
    check_non_amplification(result)
}

/// Internal engine sentinel: verify I-2 + I-7 after cascade.
///
/// Stricter than `is_conserved` (which only checks I-1/I-2). This also
/// verifies I-7 (locality) to catch algorithm bugs that alter topology.
/// Returns `Err(ConservationError)` on violation — never panics.
pub fn verify_conservation(
    original: &WaitForGraph,
    result: &WaitForGraph,
) -> Result<(), ConservationError> {
    let i2_ok = check_non_amplification(result);
    let i7_ok = check_locality(original, result);

    if i2_ok && i7_ok {
        Ok(())
    } else {
        Err(ConservationError { i2_ok, i7_ok })
    }
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

    let first = cascade_engine(graph, Some(max_depth)).expect("I-5: first cascade failed");
    let second = cascade_engine(&first, Some(max_depth)).expect("I-5: second cascade failed");

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

/// I-6: Depth monotonicity (simple chains only).
///
/// For simple chains (no fan-out, no concurrent waiters): increasing
/// max_depth propagates more weight downstream, so
/// `total_attributed(deep) ≤ total_attributed(shallow)`.
///
/// Does NOT hold in general because the corrected child_absorbed
/// computation (`prop_down + child_blame`) can be less than
/// `window.duration()` when fan-out (target_count > 1) or concurrent
/// waiters divide the transfer amount. This means deeper recursion
/// may propagate less weight downstream than the depth-truncation
/// base case, which returns full `(0, window.duration())`.
///
/// Test-only — runs cascade at two depths. Only valid on simple chains.
pub fn check_depth_monotonicity(graph: &WaitForGraph) -> bool {
    use super::engine::cascade_engine;

    let shallow = cascade_engine(graph, Some(2)).expect("I-6: shallow cascade failed");
    let deep = cascade_engine(graph, Some(10)).expect("I-6: deep cascade failed");

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
        assert!(is_conserved(&result));
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

        let result = cascade_engine(&g, None).unwrap();
        assert!(is_conserved(&result));
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
        assert!(!is_conserved(&bad));
    }

    #[test]
    fn verify_conservation_detects_locality_violation() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        // Add an extra edge in result — violates locality
        let mut bad = g.clone_with_reset_attribution();
        bad.add_node(ThreadId(3), NodeKind::UserThread);
        bad.add_edge(ThreadId(1), ThreadId(3), TimeWindow::new(0, 30));
        assert!(verify_conservation(&g, &bad).is_err());
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
    fn verify_conservation_returns_ok_on_valid() {
        use crate::cascade::engine::cascade_engine;
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let result = cascade_engine(&g, None).unwrap();
        assert!(verify_conservation(&g, &result).is_ok());
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
        let result = cascade_engine(&g, Some(10)).unwrap();
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

        let shallow = cascade_engine(&g, Some(2)).unwrap();
        let deep = cascade_engine(&g, Some(10)).unwrap();
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
