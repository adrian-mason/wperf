//! Sweep-line partition for overlapping outgoing edges.
//!
//! Decomposes a time window into elementary intervals where the
//! set of concurrent wait targets is constant. This is the BUG-3 fix —
//! without it, overlapping edges cause double-counting.

use std::collections::BTreeSet;

use super::types::*;
use super::wfg::WaitForGraph;

use petgraph::Direction;
use petgraph::visit::EdgeRef;

/// Decompose the outgoing edges of `node` that overlap `window` into
/// non-overlapping elementary intervals.
///
/// Algorithm: O(K log K) where K = number of outgoing edges.
/// 1. Clip each outgoing edge to its intersection with `window`
/// 2. Collect all distinct start/end points
/// 3. Sweep left-to-right: between consecutive points, emit an
///    ElementaryInterval listing active targets
///
/// Returns empty Vec if no outgoing edges overlap `window`.
pub fn sweep_line_partition(
    graph: &WaitForGraph,
    node: ThreadId,
    window: &TimeWindow,
) -> Vec<ElementaryInterval> {
    let node_idx = match graph.node_index(&node) {
        Some(idx) => idx,
        None => return Vec::new(),
    };

    // Collect clipped intervals: (clipped_window, target_tid)
    let mut clipped: Vec<(TimeWindow, ThreadId)> = Vec::new();
    for edge_ref in graph.graph.edges_directed(node_idx, Direction::Outgoing) {
        let ew = edge_ref.weight();
        if let Some(overlap) = window.overlap(&ew.time_window) {
            let target_tid = graph.graph[edge_ref.target()].tid;
            clipped.push((overlap, target_tid));
        }
    }

    if clipped.is_empty() {
        return Vec::new();
    }

    // Collect all distinct boundary points
    let mut points = BTreeSet::new();
    for (w, _) in &clipped {
        points.insert(w.start_ms);
        points.insert(w.end_ms);
    }
    let points: Vec<u64> = points.into_iter().collect(); // already sorted (BTreeSet)

    // Sweep: for each consecutive pair, find active targets
    let mut result = Vec::new();
    for pair in points.windows(2) {
        let (s, e) = (pair[0], pair[1]);
        if s >= e {
            continue;
        }
        let sub = TimeWindow::new(s, e);

        // Targets active during this sub-interval
        let mut targets: Vec<ThreadId> = clipped
            .iter()
            .filter(|(w, _)| w.start_ms <= s && w.end_ms >= e)
            .map(|(_, tid)| *tid)
            .collect();

        if targets.is_empty() {
            continue;
        }

        targets.sort(); // deterministic order
        targets.dedup();

        result.push(ElementaryInterval {
            window: sub,
            targets,
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::NodeKind;

    fn make_graph_with_outgoing(src: ThreadId, edges: &[(ThreadId, u64, u64)]) -> WaitForGraph {
        let mut g = WaitForGraph::new();
        g.add_node(src, NodeKind::UserThread);
        for &(dst, s, e) in edges {
            g.add_node(dst, NodeKind::UserThread);
            g.add_edge(src, dst, TimeWindow::new(s, e));
        }
        g
    }

    #[test]
    fn no_outgoing() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        let result = sweep_line_partition(&g, ThreadId(1), &TimeWindow::new(0, 100));
        assert!(result.is_empty());
    }

    #[test]
    fn single_edge_full_overlap() {
        let g = make_graph_with_outgoing(ThreadId(1), &[(ThreadId(2), 0, 100)]);
        let result = sweep_line_partition(&g, ThreadId(1), &TimeWindow::new(0, 100));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].window, TimeWindow::new(0, 100));
        assert_eq!(result[0].targets, vec![ThreadId(2)]);
    }

    #[test]
    fn single_edge_partial_overlap() {
        let g = make_graph_with_outgoing(ThreadId(1), &[(ThreadId(2), 20, 80)]);
        let result = sweep_line_partition(&g, ThreadId(1), &TimeWindow::new(0, 100));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].window, TimeWindow::new(20, 80));
    }

    #[test]
    fn two_overlapping_edges() {
        // B -> C [0, 60), B -> D [20, 80)
        // Window [0, 100)
        // Expected: [0,20)={C}, [20,60)={C,D}, [60,80)={D}
        let g =
            make_graph_with_outgoing(ThreadId(1), &[(ThreadId(2), 0, 60), (ThreadId(3), 20, 80)]);
        let result = sweep_line_partition(&g, ThreadId(1), &TimeWindow::new(0, 100));

        assert_eq!(result.len(), 3);

        assert_eq!(result[0].window, TimeWindow::new(0, 20));
        assert_eq!(result[0].targets, vec![ThreadId(2)]);

        assert_eq!(result[1].window, TimeWindow::new(20, 60));
        assert_eq!(result[1].targets, vec![ThreadId(2), ThreadId(3)]);

        assert_eq!(result[2].window, TimeWindow::new(60, 80));
        assert_eq!(result[2].targets, vec![ThreadId(3)]);
    }

    #[test]
    fn non_overlapping_edges() {
        // B -> C [0, 30), B -> D [50, 80)
        let g =
            make_graph_with_outgoing(ThreadId(1), &[(ThreadId(2), 0, 30), (ThreadId(3), 50, 80)]);
        let result = sweep_line_partition(&g, ThreadId(1), &TimeWindow::new(0, 100));

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].targets, vec![ThreadId(2)]);
        assert_eq!(result[1].targets, vec![ThreadId(3)]);
    }

    #[test]
    fn total_coverage_equals_sum_of_intervals() {
        let g =
            make_graph_with_outgoing(ThreadId(1), &[(ThreadId(2), 0, 60), (ThreadId(3), 20, 80)]);
        let result = sweep_line_partition(&g, ThreadId(1), &TimeWindow::new(0, 100));
        let total: u64 = result.iter().map(|i| i.window.duration()).sum();
        // Coverage = [0,80) = 80ms (union of [0,60) and [20,80))
        assert_eq!(total, 80);
    }
}
