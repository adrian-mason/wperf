//! Tarjan SCC identification and condensation DAG construction.
//!
//! Step 5 of the 7-step pipeline (§3.5).
//! petgraph::algo::tarjan_scc returns components in reverse topological
//! order (sink SCCs first).

use std::collections::BTreeMap;

use petgraph::Direction;
use petgraph::algo::tarjan_scc;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::Serialize;

use crate::graph::types::*;
use crate::graph::wfg::WaitForGraph;

/// A strongly connected component.
#[derive(Debug, Clone)]
pub struct Scc {
    pub members: Vec<ThreadId>, // sorted for determinism
}

/// Super-node in the condensation DAG.
#[derive(Debug, Clone, Serialize)]
pub struct SuperNode {
    pub scc_index: usize,
    pub members: Vec<ThreadId>,
    pub weight: u64, // filled by MAX heuristic (#17)
}

/// The condensation DAG — acyclic graph of super-nodes.
/// Edges carry the max attributed_delay among parallel cross-SCC edges.
pub struct CondensationDag {
    pub dag: DiGraph<SuperNode, u64>,
    node_map: BTreeMap<ThreadId, NodeIndex>,
}

impl CondensationDag {
    pub fn super_node(&self, idx: NodeIndex) -> &SuperNode {
        &self.dag[idx]
    }

    pub fn super_node_mut(&mut self, idx: NodeIndex) -> &mut SuperNode {
        &mut self.dag[idx]
    }

    /// Which super-node does this thread belong to?
    pub fn scc_of(&self, tid: &ThreadId) -> Option<NodeIndex> {
        self.node_map.get(tid).copied()
    }

    pub fn node_count(&self) -> usize {
        self.dag.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.dag.edge_count()
    }

    /// All super-nodes, sorted by index for determinism.
    pub fn all_super_nodes(&self) -> Vec<(NodeIndex, &SuperNode)> {
        let mut result: Vec<_> = self
            .dag
            .node_indices()
            .map(|idx| (idx, &self.dag[idx]))
            .collect();
        result.sort_by_key(|(idx, _)| *idx);
        result
    }

    /// Out-degree of a super-node in the condensation DAG.
    pub fn out_degree(&self, idx: NodeIndex) -> usize {
        self.dag.edges_directed(idx, Direction::Outgoing).count()
    }

    /// Sink super-nodes (out_degree == 0).
    pub fn sinks(&self) -> Vec<NodeIndex> {
        self.dag
            .node_indices()
            .filter(|&idx| self.out_degree(idx) == 0)
            .collect()
    }
}

/// Find all strongly connected components using Tarjan's algorithm.
/// Returns SCCs in reverse topological order (sink SCCs first).
pub fn find_sccs(graph: &WaitForGraph) -> Vec<Scc> {
    let sccs = tarjan_scc(&graph.graph);
    sccs.into_iter()
        .map(|component| {
            let mut members: Vec<ThreadId> = component
                .into_iter()
                .map(|idx| graph.thread_id(idx))
                .collect();
            members.sort();
            Scc { members }
        })
        .collect()
}

/// Build an acyclic condensation DAG from the Wait-For Graph.
///
/// Each SCC becomes a super-node (weight=0, filled by heuristic).
/// Cross-SCC edges become DAG edges with weight = max attributed_delay
/// among all parallel edges between the same pair of SCCs.
pub fn build_condensation(graph: &WaitForGraph) -> CondensationDag {
    let sccs = find_sccs(graph);

    let mut dag: DiGraph<SuperNode, u64> = DiGraph::new();
    let mut node_map: BTreeMap<ThreadId, NodeIndex> = BTreeMap::new();

    // Create super-nodes
    for (i, scc) in sccs.iter().enumerate() {
        let idx = dag.add_node(SuperNode {
            scc_index: i,
            members: scc.members.clone(),
            weight: 0,
        });
        for &tid in &scc.members {
            node_map.insert(tid, idx);
        }
    }

    // Add cross-SCC edges, deduplicating with max weight
    let mut edge_weights: BTreeMap<(NodeIndex, NodeIndex), u64> = BTreeMap::new();
    for (_, src_tid, dst_tid, ew) in graph.all_edges() {
        let src_scc = node_map[&src_tid];
        let dst_scc = node_map[&dst_tid];
        if src_scc != dst_scc {
            let entry = edge_weights.entry((src_scc, dst_scc)).or_insert(0);
            *entry = (*entry).max(ew.attributed_delay_ms);
        }
    }

    for ((src, dst), weight) in edge_weights {
        dag.add_edge(src, dst, weight);
    }

    CondensationDag { dag, node_map }
}

/// Get the internal edges of an SCC (edges where both endpoints are members).
pub fn internal_edges<'a>(
    graph: &'a WaitForGraph,
    scc: &Scc,
) -> Vec<(ThreadId, ThreadId, &'a EdgeWeight)> {
    let members: std::collections::BTreeSet<ThreadId> = scc.members.iter().copied().collect();

    let mut result = Vec::new();
    for &src_tid in &scc.members {
        let src_idx = graph
            .node_index(&src_tid)
            .expect("SCC member must exist in the graph");
        for (_, dst_tid, ew) in graph.outgoing_edges(src_idx) {
            if members.contains(&dst_tid) {
                result.push((src_tid, dst_tid, ew));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acyclic_graph_all_singleton_sccs() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

        let sccs = find_sccs(&g);
        assert_eq!(sccs.len(), 3, "acyclic graph → 3 singleton SCCs");
        for scc in &sccs {
            assert_eq!(scc.members.len(), 1);
        }
    }

    #[test]
    fn simple_cycle_one_scc() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(1), TimeWindow::new(0, 100));

        let sccs = find_sccs(&g);
        assert_eq!(sccs.len(), 1, "mutual cycle → 1 SCC");
        assert_eq!(sccs[0].members.len(), 2);
    }

    #[test]
    fn three_node_cycle() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(3), ThreadId(1), TimeWindow::new(0, 100));

        let sccs = find_sccs(&g);
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0].members.len(), 3);
    }

    #[test]
    fn mixed_cycle_and_chain() {
        // A→B→C→B (cycle), C→D (chain out)
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread); // A
        g.add_node(ThreadId(2), NodeKind::UserThread); // B
        g.add_node(ThreadId(3), NodeKind::UserThread); // C
        g.add_node(ThreadId(4), NodeKind::UserThread); // D
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100)); // A→B
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 100)); // B→C
        g.add_edge(ThreadId(3), ThreadId(2), TimeWindow::new(0, 100)); // C→B (cycle)
        g.add_edge(ThreadId(3), ThreadId(4), TimeWindow::new(0, 50)); // C→D

        let sccs = find_sccs(&g);
        // {B,C} is one SCC, {A} and {D} are singletons
        assert_eq!(sccs.len(), 3);
        let cycle_scc = sccs.iter().find(|s| s.members.len() == 2).unwrap();
        assert!(cycle_scc.members.contains(&ThreadId(2)));
        assert!(cycle_scc.members.contains(&ThreadId(3)));
    }

    #[test]
    fn condensation_acyclic() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

        let cdag = build_condensation(&g);
        // Acyclic: 3 super-nodes, 2 edges
        assert_eq!(cdag.node_count(), 3);
        assert_eq!(cdag.edge_count(), 2);
    }

    #[test]
    fn condensation_with_cycle() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread); // A
        g.add_node(ThreadId(2), NodeKind::UserThread); // B
        g.add_node(ThreadId(3), NodeKind::UserThread); // C
        g.add_node(ThreadId(4), NodeKind::UserThread); // D
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100)); // A→B
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 100)); // B→C
        g.add_edge(ThreadId(3), ThreadId(2), TimeWindow::new(0, 100)); // C→B
        g.add_edge(ThreadId(3), ThreadId(4), TimeWindow::new(0, 50)); // C→D

        let cdag = build_condensation(&g);
        // 3 super-nodes: {A}, {B,C}, {D}
        assert_eq!(cdag.node_count(), 3);
        // Edges: A→{B,C}, {B,C}→D
        assert_eq!(cdag.edge_count(), 2);

        // Verify B and C are in the same super-node
        let b_scc = cdag.scc_of(&ThreadId(2)).unwrap();
        let c_scc = cdag.scc_of(&ThreadId(3)).unwrap();
        assert_eq!(b_scc, c_scc);
    }

    #[test]
    fn condensation_sinks() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(1), ThreadId(3), TimeWindow::new(0, 50));

        let cdag = build_condensation(&g);
        let sinks = cdag.sinks();
        // T2 and T3 are sinks (no outgoing edges)
        assert_eq!(sinks.len(), 2);
    }

    #[test]
    fn internal_edges_of_cycle() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(1), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(1), ThreadId(3), TimeWindow::new(0, 50));

        let sccs = find_sccs(&g);
        let cycle = sccs.iter().find(|s| s.members.len() == 2).unwrap();
        let int_edges = internal_edges(&g, cycle);
        // T1→T2 and T2→T1 are internal
        assert_eq!(int_edges.len(), 2);
    }
}
