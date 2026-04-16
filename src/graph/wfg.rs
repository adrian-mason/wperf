//! Wait-For Graph — wraps `petgraph::DiGraph` with ThreadId-based lookup.
//!
//! Uses `BTreeMap` for deterministic iteration order (ADR-007).

use std::collections::BTreeMap;

use petgraph::Direction;
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};
use petgraph::visit::EdgeRef;

use super::types::{EdgeWeight, NodeKind, NodeWeight, ThreadId, TimeWindow, WaitType};

/// The Wait-For Graph. Directed graph where:
/// - Nodes = threads (or pseudo-threads)
/// - Edges = "waiter → waitee" with time window and weight
#[derive(Debug)]
pub struct WaitForGraph {
    pub(crate) graph: DiGraph<NodeWeight, EdgeWeight>,
    pub(crate) node_map: BTreeMap<ThreadId, NodeIndex>,
}

impl WaitForGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            node_map: BTreeMap::new(),
        }
    }

    /// Add a node (thread). Returns the `NodeIndex`. Idempotent — returns
    /// existing index if tid already present.
    pub fn add_node(&mut self, tid: ThreadId, kind: NodeKind) -> NodeIndex {
        if let Some(&idx) = self.node_map.get(&tid) {
            return idx;
        }
        let idx = self.graph.add_node(NodeWeight { tid, kind });
        self.node_map.insert(tid, idx);
        idx
    }

    /// Add a directed edge: `src` waits for `dst` during `window`.
    pub fn add_edge(&mut self, src: ThreadId, dst: ThreadId, window: TimeWindow) -> EdgeIndex {
        let src_idx = *self.node_map.get(&src).expect("src node not in graph");
        let dst_idx = *self.node_map.get(&dst).expect("dst node not in graph");
        self.graph
            .add_edge(src_idx, dst_idx, EdgeWeight::new(window))
    }

    /// Add a directed edge with explicit wait type annotation.
    pub fn add_edge_with_wait_type(
        &mut self,
        src: ThreadId,
        dst: ThreadId,
        window: TimeWindow,
        wait_type: WaitType,
    ) -> EdgeIndex {
        let src_idx = *self.node_map.get(&src).expect("src node not in graph");
        let dst_idx = *self.node_map.get(&dst).expect("dst node not in graph");
        self.graph.add_edge(
            src_idx,
            dst_idx,
            EdgeWeight::with_wait_type(window, wait_type),
        )
    }

    /// Get the `ThreadId` for a `NodeIndex`.
    pub fn thread_id(&self, idx: NodeIndex) -> ThreadId {
        self.graph[idx].tid
    }

    /// Get `NodeIndex` for a `ThreadId`.
    pub fn node_index(&self, tid: &ThreadId) -> Option<NodeIndex> {
        self.node_map.get(tid).copied()
    }

    /// Get all outgoing edges from `node` as (`EdgeIndex`, `dst_ThreadId`, &`EdgeWeight`).
    pub fn outgoing_edges(&self, node: NodeIndex) -> Vec<(EdgeIndex, ThreadId, &EdgeWeight)> {
        self.graph
            .edges_directed(node, Direction::Outgoing)
            .map(|e| (e.id(), self.graph[e.target()].tid, e.weight()))
            .collect()
    }

    /// Get all incoming edges to `node`.
    pub fn incoming_edges(&self, node: NodeIndex) -> Vec<(EdgeIndex, ThreadId, &EdgeWeight)> {
        self.graph
            .edges_directed(node, Direction::Incoming)
            .map(|e| (e.id(), self.graph[e.source()].tid, e.weight()))
            .collect()
    }

    /// Iterate all edges as (`EdgeIndex`, `src_tid`, `dst_tid`, &`EdgeWeight`).
    /// BTreeMap-ordered by source tid for determinism.
    pub fn all_edges(&self) -> Vec<(EdgeIndex, ThreadId, ThreadId, &EdgeWeight)> {
        let mut result: Vec<_> = self
            .graph
            .edge_indices()
            .map(|eidx| {
                let (src, dst) = self.graph.edge_endpoints(eidx).unwrap();
                (
                    eidx,
                    self.graph[src].tid,
                    self.graph[dst].tid,
                    &self.graph[eidx],
                )
            })
            .collect();
        // Sort by (src_tid, dst_tid) for determinism
        result.sort_by_key(|(_, s, d, _)| (*s, *d));
        result
    }

    /// Total number of nodes.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Total number of edges.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Get edge weight by index.
    pub fn edge_weight(&self, idx: EdgeIndex) -> &EdgeWeight {
        &self.graph[idx]
    }

    /// Get mutable edge weight by index.
    pub fn edge_weight_mut(&mut self, idx: EdgeIndex) -> &mut EdgeWeight {
        &mut self.graph[idx]
    }

    /// Get node weight by index.
    pub fn node_weight(&self, idx: NodeIndex) -> &NodeWeight {
        &self.graph[idx]
    }

    /// All node indices, sorted by `ThreadId` for determinism.
    pub fn node_indices(&self) -> Vec<NodeIndex> {
        self.node_map.values().copied().collect()
    }

    /// Clone the graph structure with the same topology but reset
    /// `attributed_delay_ms` to `raw_wait_ms` on all edges.
    pub fn clone_with_reset_attribution(&self) -> Self {
        let mut new = Self::new();
        // Clone nodes
        for (&tid, &idx) in &self.node_map {
            new.add_node(tid, self.graph[idx].kind);
        }
        // Clone edges
        for eidx in self.graph.edge_indices() {
            let (src, dst) = self.graph.edge_endpoints(eidx).unwrap();
            let src_tid = self.graph[src].tid;
            let dst_tid = self.graph[dst].tid;
            let weight = &self.graph[eidx];
            new.add_edge(src_tid, dst_tid, weight.time_window);
        }
        new
    }

    /// Sum of all `raw_wait_ms` across all edges.
    pub fn total_raw_wait(&self) -> u64 {
        self.graph
            .edge_indices()
            .map(|e| self.graph[e].raw_wait_ms)
            .sum()
    }

    /// Returns true if the graph has no directed cycles.
    pub fn is_acyclic(&self) -> bool {
        petgraph::algo::toposort(&self.graph, None).is_ok()
    }

    /// Sum of all `attributed_delay_ms` across all edges.
    pub fn total_attributed(&self) -> u64 {
        self.graph
            .edge_indices()
            .map(|e| self.graph[e].attributed_delay_ms)
            .sum()
    }
}

impl Default for WaitForGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_graph() -> WaitForGraph {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g
    }

    #[test]
    fn add_nodes_and_edges() {
        let g = simple_graph();
        assert_eq!(g.node_count(), 2);
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn outgoing_edges() {
        let g = simple_graph();
        let n1 = g.node_index(&ThreadId(1)).unwrap();
        let out = g.outgoing_edges(n1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, ThreadId(2));
        assert_eq!(out[0].2.raw_wait_ms, 100);
    }

    #[test]
    fn total_raw_wait() {
        let g = simple_graph();
        assert_eq!(g.total_raw_wait(), 100);
    }

    #[test]
    fn idempotent_add_node() {
        let mut g = WaitForGraph::new();
        let a = g.add_node(ThreadId(1), NodeKind::UserThread);
        let b = g.add_node(ThreadId(1), NodeKind::UserThread);
        assert_eq!(a, b);
        assert_eq!(g.node_count(), 1);
    }

    #[test]
    fn is_acyclic_linear() {
        let g = simple_graph();
        assert!(g.is_acyclic());
    }

    #[test]
    fn is_acyclic_cycle() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));
        g.add_edge(ThreadId(2), ThreadId(1), TimeWindow::new(0, 50));
        assert!(!g.is_acyclic());
    }

    #[test]
    fn total_attributed_matches_raw_before_cascade() {
        let g = simple_graph();
        // Before cascade, attributed == raw
        assert_eq!(g.total_attributed(), g.total_raw_wait());
        assert_eq!(g.total_attributed(), 100);
    }
}
