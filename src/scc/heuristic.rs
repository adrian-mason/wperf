//! MAX heuristic for super-node weights (step 6, §3.5).
//!
//! Super-node weight = `MAX(attributed_delay` of all internal edges).
//! For singleton SCCs, weight = max `attributed_delay` of incident edges.
//! This is explicitly a sorting heuristic, not mathematical truth
//! (ADR-008).

use crate::graph::wfg::WaitForGraph;

use super::tarjan::{CondensationDag, Scc, internal_edges};

/// Compute MAX heuristic weight for an SCC.
/// Returns the maximum `attributed_delay_ms` among all internal edges.
/// For singleton SCCs with no internal edges, returns the max
/// `attributed_delay` of any edge touching the member.
pub fn max_heuristic_weight(graph: &WaitForGraph, scc: &Scc) -> u64 {
    let int_edges = internal_edges(graph, scc);

    if !int_edges.is_empty() {
        return int_edges
            .iter()
            .map(|(_, _, ew)| ew.attributed_delay_ms)
            .max()
            .unwrap_or(0);
    }

    // Singleton: use max of all incident edges (in or out)
    if scc.members.len() == 1 {
        let tid = &scc.members[0];
        if let Some(idx) = graph.node_index(tid) {
            let out_max = graph
                .outgoing_edges(idx)
                .iter()
                .map(|(_, _, ew)| ew.attributed_delay_ms)
                .max()
                .unwrap_or(0);
            let in_max = graph
                .incoming_edges(idx)
                .iter()
                .map(|(_, _, ew)| ew.attributed_delay_ms)
                .max()
                .unwrap_or(0);
            return out_max.max(in_max);
        }
    }

    0
}

/// Apply MAX heuristic to all super-nodes in the condensation DAG.
pub fn apply_max_heuristic(cdag: &mut CondensationDag, graph: &WaitForGraph) {
    let node_indices: Vec<_> = cdag.dag.node_indices().collect();
    for idx in node_indices {
        let scc = Scc {
            members: cdag.super_node(idx).members.clone(),
        };
        let weight = max_heuristic_weight(graph, &scc);
        cdag.super_node_mut(idx).weight = weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cascade::engine::cascade_engine;
    use crate::graph::types::*;
    use crate::scc::tarjan::build_condensation;

    #[test]
    fn singleton_weight_from_incident_edges() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let result = cascade_engine(&g, None).unwrap();
        let scc = Scc {
            members: vec![ThreadId(2)],
        };
        // T2 has incoming edge with attributed=50
        assert_eq!(max_heuristic_weight(&result, &scc), 50);
    }

    #[test]
    fn cycle_weight_from_internal_edges() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(1), TimeWindow::new(0, 60));

        let scc = Scc {
            members: vec![ThreadId(1), ThreadId(2)],
        };
        // Internal edges: T1→T2 (raw=100), T2→T1 (raw=60)
        // Before cascade, attributed = raw
        let w = max_heuristic_weight(&g, &scc);
        assert_eq!(w, 100, "MAX of internal edges");
    }

    #[test]
    fn apply_heuristic_sets_weights() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

        let result = cascade_engine(&g, None).unwrap();
        let mut cdag = build_condensation(&result);
        apply_max_heuristic(&mut cdag, &result);

        // All nodes should have non-zero weights
        for (_, sn) in cdag.all_super_nodes() {
            assert!(sn.weight > 0, "super-node {:?} has zero weight", sn.members);
        }
    }

    #[test]
    fn heuristic_on_figure4() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::PseudoNic);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

        let result = cascade_engine(&g, None).unwrap();
        let mut cdag = build_condensation(&result);
        apply_max_heuristic(&mut cdag, &result);

        // T3 (Network) is the sink with highest attributed (80ms)
        let t3_idx = cdag.scc_of(&ThreadId(3)).unwrap();
        assert_eq!(cdag.super_node(t3_idx).weight, 80);

        // T2 (Parser) has attributed=20 on incoming edge from T1
        let t2_idx = cdag.scc_of(&ThreadId(2)).unwrap();
        assert_eq!(cdag.super_node(t2_idx).weight, 80);
        // T2's max incident edge: outgoing T2→T3 (attributed=80)
    }

    #[test]
    fn isolated_node_zero_weight() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);

        let scc = Scc {
            members: vec![ThreadId(1)],
        };
        assert_eq!(max_heuristic_weight(&g, &scc), 0);
    }
}
