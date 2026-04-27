//! Cascade redistribution engine.
//!
//! Implements Step 4 of the algorithm pipeline (§3.4).
//! Pure function: takes immutable graph, returns new graph with
//! `attributed_delay_ms` computed.
//!
//! Each edge is processed independently:
//!   `attributed_delay` = `raw_wait` - `propagated_downstream`
//! This means `attributed_delay` represents the DIRECT fault of the
//! destination node, excluding what deeper nodes are responsible for.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use petgraph::graph::EdgeIndex;

use crate::graph::sweep::sweep_line_partition;
use crate::graph::types::{EdgeKind, ThreadId, TimeWindow};
use crate::graph::wfg::WaitForGraph;

use super::invariants;
pub use super::invariants::InvariantError;

const DEFAULT_MAX_DEPTH: u32 = 10;

/// Run cascade redistribution on the Wait-For Graph.
///
/// Each edge's `attributed_delay_ms` is set to `raw_wait - propagated`,
/// where `propagated` is the weight explained by deeper nodes.
///
/// Returns `Err(InvariantError)` if invariant checks fail.
/// Never panics — safe for release builds.
pub fn cascade_engine(
    graph: &WaitForGraph,
    max_depth: Option<u32>,
) -> Result<WaitForGraph, InvariantError> {
    let max_depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH);

    let mut attribution: BTreeMap<EdgeIndex, u64> = BTreeMap::new();

    for (eidx, src_tid, dst_tid, ew) in graph.all_edges() {
        let window = ew.time_window;
        let raw_wait = ew.raw_wait_ms;

        let mut path = BTreeSet::new();
        path.insert(src_tid);

        let (propagated, _self_blame) =
            compute_cascade(graph, dst_tid, &window, 1, max_depth, &mut path);

        let attributed = raw_wait.saturating_sub(propagated);
        attribution.insert(eidx, attributed);
    }

    let mut result = graph.clone_with_reset_attribution();
    for (eidx, attributed) in &attribution {
        result.edge_weight_mut(*eidx).attributed_delay_ms = *attributed;
    }

    // Production sentinel — verify I-2 ∧ I-7 postconditions (never panics)
    invariants::verify_engine_postconditions(graph, &result)?;

    // I-3: Non-negativity (trivially true for u64, documents intent)
    debug_assert!(
        invariants::check_non_negativity(&result),
        "I-3 VIOLATION: negative attributed delay"
    );

    // I-4: Termination (topology preserved)
    debug_assert!(
        invariants::check_termination(graph, &result),
        "I-4 VIOLATION: cascade changed graph topology"
    );

    Ok(result)
}

/// Recursive cascade computation (ADR-007 pseudocode).
///
/// Returns `(total_propagated, self_blame)`:
/// - `total_propagated`: weight pushed to deeper nodes via outgoing edges
/// - `self_blame`: time in `window` not covered by any outgoing edge
///
/// Internal tuple accounting: `propagated + self_blame <= window.duration()`.
/// Equality holds when no fan-out or concurrent-waiter scaling is applied;
/// integer division truncation may cause the sum to be strictly less.
/// Note: I-1 (conservation) was retired by ADR-016. This property is
/// by-construction (attributed = raw - propagated), not an external invariant.
fn compute_cascade(
    graph: &WaitForGraph,
    current: ThreadId,
    window: &TimeWindow,
    depth: u32,
    max_depth: u32,
    path: &mut BTreeSet<ThreadId>,
) -> (u64, u64) {
    if depth >= max_depth || path.contains(&current) {
        return (0, window.duration());
    }

    let intervals = sweep_line_partition(graph, current, window);
    if intervals.is_empty() {
        return (0, window.duration());
    }

    path.insert(current);
    let mut total_propagated: u64 = 0;
    let mut coverage: u64 = 0;

    for interval in &intervals {
        let target_count = interval.targets.len() as u64;
        coverage += interval.window.duration();

        for &next_node in &interval.targets {
            let (prop_down, child_blame) = compute_cascade(
                graph,
                next_node,
                &interval.window,
                depth + 1,
                max_depth,
                path,
            );

            let child_absorbed = prop_down + child_blame;

            let external_waiters = count_concurrent_waiters(graph, next_node, &interval.window);
            let scale_target = target_count.max(1);
            let scale_external = external_waiters.max(1);

            let transfer = child_absorbed / scale_target / scale_external;
            total_propagated += transfer;
        }
    }

    path.remove(&current);
    let self_blame = window.duration().saturating_sub(coverage);
    (total_propagated, self_blame)
}

fn count_concurrent_waiters(graph: &WaitForGraph, target: ThreadId, window: &TimeWindow) -> u64 {
    let Some(node_idx) = graph.node_index(&target) else {
        return 1;
    };

    // ADR-009 Amendment 2026-04-25 (final-design.md §3.3): synthetic
    // closure-return edges (`EdgeKind::SyntheticClosureReturn`) are
    // cascade-terminal and must NOT count as concurrent waiters; they
    // represent SCC closure bookkeeping, not real wait dependencies.
    // Skipping them here is the second of the two filter sites
    // mandated by the amendment (the first is in
    // `sweep_line_partition`). Applying only one of the two leaves
    // the divisor polluted by bookkeeping edges and silently halves
    // cascade transfer on the forward direction (PR #120 5-way
    // thread, Probe Gap 5).
    let count = graph
        .incoming_edges(node_idx)
        .iter()
        .filter(|(_, _, ew)| ew.kind != EdgeKind::SyntheticClosureReturn)
        .filter(|(_, _, ew)| ew.time_window.overlap(window).is_some())
        .map(|(_, src_tid, _)| *src_tid)
        .collect::<BTreeSet<_>>()
        .len();

    if count == 0 { 1 } else { count as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::NodeKind;

    fn figure4_graph() -> WaitForGraph {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));
        g
    }

    #[test]
    fn cascade_single_edge() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let result = cascade_engine(&g, None).unwrap();
        let edges = result.all_edges();
        assert_eq!(edges[0].3.attributed_delay_ms, 50);
    }

    #[test]
    fn cascade_figure4() {
        let g = figure4_graph();
        let result = cascade_engine(&g, None).unwrap();

        let edges = result.all_edges();
        let user_parser = edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
            .unwrap();
        let parser_network = edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(2) && *d == ThreadId(3))
            .unwrap();

        // User→Parser: raw=100, propagated 80 to Network → attributed=20
        assert_eq!(user_parser.3.attributed_delay_ms, 20, "User→Parser");
        // Parser→Network: raw=80, Network is leaf → attributed=80
        assert_eq!(parser_network.3.attributed_delay_ms, 80, "Parser→Network");
    }

    #[test]
    fn cascade_extended_3node() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_node(ThreadId(4), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100)); // User→Parser
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100)); // Parser→Network
        g.add_edge(ThreadId(3), ThreadId(4), TimeWindow::new(50, 100)); // Network→Disk

        let result = cascade_engine(&g, None).unwrap();
        let edges = result.all_edges();

        let up = edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
            .unwrap();
        let pn = edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(2) && *d == ThreadId(3))
            .unwrap();
        let nd = edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(3) && *d == ThreadId(4))
            .unwrap();

        // User→Parser: raw=100, propagated=80 → attributed=20
        assert_eq!(up.3.attributed_delay_ms, 20);
        // Parser→Network: raw=80, propagated=50 → attributed=30
        assert_eq!(pn.3.attributed_delay_ms, 30);
        // Network→Disk: raw=50, leaf → attributed=50
        assert_eq!(nd.3.attributed_delay_ms, 50);
    }

    #[test]
    fn cascade_leaf_nonzero() {
        let g = figure4_graph();
        let result = cascade_engine(&g, None).unwrap();
        let edges = result.all_edges();
        let parser_network = edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(2) && *d == ThreadId(3))
            .unwrap();
        assert!(parser_network.3.attributed_delay_ms > 0);
    }

    #[test]
    fn cascade_invariants_ok() {
        let g = figure4_graph();
        let result = cascade_engine(&g, None).unwrap();
        assert!(invariants::invariants_ok(&g, &result));
    }

    #[test]
    fn invariants_ok_false_on_bad_graph() {
        let g = figure4_graph();
        let mut result = cascade_engine(&g, None).unwrap();
        // Corrupt: set attributed > raw on first edge
        let edges = result.all_edges();
        let eidx = edges[0].0;
        result.edge_weight_mut(eidx).attributed_delay_ms = 999;
        assert!(!invariants::invariants_ok(&g, &result));
    }

    #[test]
    fn concurrent_waiters_divides_weight() {
        // T1→T2 [0,100), T2→T3 [0,100), T4→T3 [0,100)
        // T3 has 2 incoming edges (from T2 and T4) → external_waiters=2
        // When cascading T1→T2, T2's child T3 has 2 concurrent waiters
        // → transfer is divided by 2
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_node(ThreadId(4), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(4), ThreadId(3), TimeWindow::new(0, 100));

        let result = cascade_engine(&g, None).unwrap();
        let edges = result.all_edges();

        let e12 = edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
            .unwrap();

        // Without concurrent_waiters: propagated=100, attributed=0
        // With concurrent_waiters=2: propagated=100/2=50, attributed=50
        assert_eq!(e12.3.attributed_delay_ms, 50, "T1→T2 with 2 waiters on T3");
    }

    #[test]
    fn depth_limit_changes_result() {
        // Chain: T1→T2→T3→T4, overlapping windows
        // With max_depth=1: no propagation (depth starts at 1, immediately hits limit)
        // With max_depth=10: full propagation
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 100));

        let shallow = cascade_engine(&g, Some(1)).unwrap();
        let deep = cascade_engine(&g, Some(10)).unwrap();

        let s_edges = shallow.all_edges();
        let d_edges = deep.all_edges();

        let s12 = s_edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
            .unwrap();
        let d12 = d_edges
            .iter()
            .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
            .unwrap();

        // max_depth=1: compute_cascade called with depth=1, hits limit immediately → attributed=raw=100
        assert_eq!(s12.3.attributed_delay_ms, 100, "depth=1 → no cascade");
        // max_depth=10: full cascade → attributed < raw
        assert!(
            d12.3.attributed_delay_ms < 100,
            "depth=10 → cascade reduces attribution"
        );
    }

    #[test]
    fn total_attributed_less_than_raw() {
        let g = figure4_graph();
        let result = cascade_engine(&g, None).unwrap();
        // Total attributed should be less than total raw (weight absorbed by cascade)
        assert!(result.total_attributed() < g.total_raw_wait());
        assert!(result.total_attributed() > 0);
    }

    #[test]
    fn node_indices_returns_all_nodes() {
        let g = figure4_graph();
        let indices = g.node_indices();
        assert_eq!(indices.len(), 3);
    }
}
