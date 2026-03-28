//! Cascade redistribution engine.
//!
//! Implements Step 4 of the algorithm pipeline (§3.4).
//! Pure function: takes immutable graph, returns new graph with
//! attributed_delay_ms computed.
//!
//! Each edge is processed independently:
//!   attributed_delay = raw_wait - propagated_downstream
//! This means attributed_delay represents the DIRECT fault of the
//! destination node, excluding what deeper nodes are responsible for.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use petgraph::graph::EdgeIndex;

use crate::graph::sweep::sweep_line_partition;
use crate::graph::types::*;
use crate::graph::wfg::WaitForGraph;

use super::invariants;

const DEFAULT_MAX_DEPTH: u32 = 10;

/// Run cascade redistribution on the Wait-For Graph.
///
/// Each edge's `attributed_delay_ms` is set to `raw_wait - propagated`,
/// where `propagated` is the weight explained by deeper nodes.
///
/// Invariant checks:
/// - I-2 (non-amplification): attributed ≤ raw per edge — always checked
/// - I-3 (non-negativity): attributed ≥ 0 — trivially true for u64
/// - I-7 (locality): checked in debug builds
pub fn cascade_engine(graph: &WaitForGraph, max_depth: Option<u32>) -> WaitForGraph {
    let max_depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH);

    let mut attribution: BTreeMap<EdgeIndex, u64> = BTreeMap::new();

    for (eidx, src_tid, dst_tid, ew) in graph.all_edges() {
        let window = ew.time_window;
        let raw_wait = ew.raw_wait_ms;

        let mut path = BTreeSet::new();
        path.insert(src_tid);

        let propagated =
            compute_cascade(graph, dst_tid, &window, 1, max_depth, &mut path);

        let attributed = raw_wait.saturating_sub(propagated);
        attribution.insert(eidx, attributed);
    }

    let mut result = graph.clone_with_reset_attribution();
    for (eidx, attributed) in &attribution {
        result.edge_weight_mut(*eidx).attributed_delay_ms = *attributed;
    }

    // I-1: Weight Conservation — production sentinel (always runs)
    invariants::assert_weight_conserved(graph, &result);

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

    result
}

/// Check if the cascade result is conserved (non-panicking).
/// Delegates to invariants::is_conserved (I-2 + I-7).
pub fn is_conserved(original: &WaitForGraph, result: &WaitForGraph) -> bool {
    invariants::is_conserved(original, result)
}

/// Recursive cascade computation (ADR-007 pseudocode).
/// Returns total weight propagated to deeper nodes.
fn compute_cascade(
    graph: &WaitForGraph,
    current: ThreadId,
    window: &TimeWindow,
    depth: u32,
    max_depth: u32,
    path: &mut BTreeSet<ThreadId>,
) -> u64 {
    if depth >= max_depth || path.contains(&current) {
        return 0;
    }

    let intervals = sweep_line_partition(graph, current, window);
    if intervals.is_empty() {
        return 0;
    }

    path.insert(current);
    let mut total_propagated: u64 = 0;

    for interval in &intervals {
        let target_count = interval.targets.len() as u64;

        for &next_node in &interval.targets {
            let _prop_down = compute_cascade(
                graph, next_node, &interval.window, depth + 1, max_depth, path,
            );

            // NEW-BUG-1 FIX: child absorbs interval duration (prop_down + self_blame)
            let child_subtree_absorbed = interval.window.duration();

            let external_waiters = count_concurrent_waiters(graph, next_node, &interval.window);
            let scale_target = target_count.max(1);
            let scale_external = external_waiters.max(1);

            let transfer = child_subtree_absorbed / scale_target / scale_external;
            total_propagated += transfer;
        }
    }

    path.remove(&current);
    total_propagated
}

fn count_concurrent_waiters(
    graph: &WaitForGraph,
    target: ThreadId,
    window: &TimeWindow,
) -> u64 {
    let node_idx = match graph.node_index(&target) {
        Some(idx) => idx,
        None => return 1,
    };

    let count = graph
        .incoming_edges(node_idx)
        .iter()
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

        let result = cascade_engine(&g, None);
        let edges = result.all_edges();
        assert_eq!(edges[0].3.attributed_delay_ms, 50);
    }

    #[test]
    fn cascade_figure4() {
        let g = figure4_graph();
        let result = cascade_engine(&g, None);

        let edges = result.all_edges();
        let user_parser = edges.iter().find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2)).unwrap();
        let parser_network = edges.iter().find(|(_, s, d, _)| *s == ThreadId(2) && *d == ThreadId(3)).unwrap();

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

        let result = cascade_engine(&g, None);
        let edges = result.all_edges();

        let up = edges.iter().find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2)).unwrap();
        let pn = edges.iter().find(|(_, s, d, _)| *s == ThreadId(2) && *d == ThreadId(3)).unwrap();
        let nd = edges.iter().find(|(_, s, d, _)| *s == ThreadId(3) && *d == ThreadId(4)).unwrap();

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
        let result = cascade_engine(&g, None);
        let edges = result.all_edges();
        let parser_network = edges.iter().find(|(_, s, d, _)| *s == ThreadId(2) && *d == ThreadId(3)).unwrap();
        assert!(parser_network.3.attributed_delay_ms > 0);
    }

    #[test]
    fn cascade_no_amplification() {
        let g = figure4_graph();
        let result = cascade_engine(&g, None);
        assert!(is_conserved(&g, &result));
    }
}
