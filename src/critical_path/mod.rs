//! Critical path DP on the condensation DAG (step 7, §3.6).
//!
//! O(V+E) dynamic programming via topological sort. Extracts the
//! maximum-weight path — the critical delay chain.

use petgraph::Direction;
use petgraph::algo::toposort;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use serde::Serialize;

use crate::graph::types::ThreadId;
use crate::scc::tarjan::CondensationDag;

/// The critical path result.
#[derive(Debug, Clone, Serialize)]
pub struct CriticalPath {
    /// Ordered list of super-node member sets along the path.
    pub chain: Vec<CriticalPathNode>,
    /// Total weight of the critical path.
    pub total_weight: u64,
}

/// A node in the critical path.
#[derive(Debug, Clone, Serialize)]
pub struct CriticalPathNode {
    pub members: Vec<ThreadId>,
    pub weight: u64,
}

/// Compute the critical (longest) path through the condensation DAG.
///
/// Algorithm: topological sort + DP.
/// For each node in topo order: dist[v] = max(dist[u] + edge_weight + v.weight)
/// over all predecessors u.
///
/// Returns None if the DAG is empty.
pub fn critical_path_dp(cdag: &CondensationDag) -> Option<CriticalPath> {
    if cdag.node_count() == 0 {
        return None;
    }

    let topo = match toposort(&cdag.dag, None) {
        Ok(order) => order,
        Err(_) => {
            // Should never happen — condensation is acyclic by construction
            panic!("condensation DAG has a cycle — this is a bug");
        }
    };

    let node_count = cdag.dag.node_count();
    let mut dist: Vec<u64> = vec![0; node_count];
    let mut pred: Vec<Option<NodeIndex>> = vec![None; node_count];

    // Initialize: each node's base distance is its own weight
    for &v in &topo {
        dist[v.index()] = cdag.super_node(v).weight;
    }

    // DP: process in topological order
    for &v in &topo {
        let v_dist = dist[v.index()];

        for edge in cdag.dag.edges_directed(v, Direction::Outgoing) {
            let u = edge.target();
            let edge_w = *edge.weight();
            let candidate = v_dist + edge_w + cdag.super_node(u).weight;

            if candidate > dist[u.index()] {
                dist[u.index()] = candidate;
                pred[u.index()] = Some(v);
            }
        }
    }

    // Find the node with maximum distance
    let (end_idx, &max_dist) = dist.iter().enumerate().max_by_key(|(_, d)| *d).unwrap();
    let end = NodeIndex::new(end_idx);

    // Trace back the path
    let mut path_indices = vec![end];
    let mut current = end;
    while let Some(p) = pred[current.index()] {
        path_indices.push(p);
        current = p;
    }
    path_indices.reverse();

    let chain: Vec<CriticalPathNode> = path_indices
        .iter()
        .map(|&idx| {
            let sn = cdag.super_node(idx);
            CriticalPathNode {
                members: sn.members.clone(),
                weight: sn.weight,
            }
        })
        .collect();

    Some(CriticalPath {
        chain,
        total_weight: max_dist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cascade::engine::cascade_engine;
    use crate::graph::types::*;
    use crate::graph::wfg::WaitForGraph;
    use crate::scc::heuristic::apply_max_heuristic;
    use crate::scc::tarjan::build_condensation;

    fn run_pipeline(g: &WaitForGraph) -> CriticalPath {
        let result = cascade_engine(g, None).unwrap();
        let mut cdag = build_condensation(&result);
        apply_max_heuristic(&mut cdag, &result);
        critical_path_dp(&cdag).unwrap()
    }

    #[test]
    fn single_node() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        // No edges → isolated node

        let cdag = build_condensation(&g);
        let cp = critical_path_dp(&cdag).unwrap();
        assert_eq!(cp.chain.len(), 1);
        assert_eq!(cp.total_weight, 0);
    }

    #[test]
    fn single_edge_critical_path() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

        let cp = run_pipeline(&g);
        assert_eq!(cp.chain.len(), 2);
        assert!(cp.total_weight > 0);
    }

    #[test]
    fn figure4_critical_path() {
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::PseudoNic);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

        let cp = run_pipeline(&g);
        // Critical path should include all 3 nodes (linear chain)
        assert_eq!(cp.chain.len(), 3);
        // Last node in path should be T3 (Network) — the root cause
        let last = cp.chain.last().unwrap();
        assert!(last.members.contains(&ThreadId(3)));
    }

    #[test]
    fn branching_picks_heavier_path() {
        // A→B (50ms), A→C (80ms) — critical path should go through C
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));
        g.add_edge(ThreadId(1), ThreadId(3), TimeWindow::new(0, 80));

        let cp = run_pipeline(&g);
        // Critical path should end at T3 (heavier edge)
        let last = cp.chain.last().unwrap();
        assert!(
            last.members.contains(&ThreadId(3)),
            "critical path should pick the heavier branch"
        );
    }

    #[test]
    fn dp_arithmetic_uses_addition() {
        // Construct a condensation DAG directly to test DP arithmetic
        // without pipeline interference from heuristic weights.
        use crate::scc::tarjan::{CondensationDag, SuperNode};
        use petgraph::graph::DiGraph;

        let mut dag: DiGraph<SuperNode, u64> = DiGraph::new();
        let a = dag.add_node(SuperNode {
            scc_index: 0,
            members: vec![ThreadId(1)],
            weight: 5,
        });
        let b = dag.add_node(SuperNode {
            scc_index: 1,
            members: vec![ThreadId(2)],
            weight: 3,
        });
        let c = dag.add_node(SuperNode {
            scc_index: 2,
            members: vec![ThreadId(3)],
            weight: 7,
        });
        dag.add_edge(a, b, 10);
        dag.add_edge(b, c, 20);

        let mut node_map = std::collections::BTreeMap::new();
        node_map.insert(ThreadId(1), a);
        node_map.insert(ThreadId(2), b);
        node_map.insert(ThreadId(3), c);

        let cdag = CondensationDag { dag, node_map };
        let cp = critical_path_dp(&cdag).unwrap();
        // dist[a]=5, dist[b]=5+10+3=18, dist[c]=18+20+7=45
        assert_eq!(cp.total_weight, 45, "DP must use addition");
        assert_eq!(cp.chain.len(), 3);
    }

    #[test]
    fn empty_dag() {
        let cdag = build_condensation(&WaitForGraph::new());
        assert!(critical_path_dp(&cdag).is_none());
    }

    #[test]
    fn long_chain_linear_complexity() {
        // 100-node linear chain — should complete quickly (O(V+E))
        // Staggered windows so each node has some direct attribution
        let mut g = WaitForGraph::new();
        for i in 0..100 {
            g.add_node(ThreadId(i), NodeKind::UserThread);
        }
        for i in 0..99i64 {
            let start = (i * 10) as u64;
            let end = start + 100;
            g.add_edge(ThreadId(i), ThreadId(i + 1), TimeWindow::new(start, end));
        }

        let cp = run_pipeline(&g);
        // Critical path should traverse the full chain
        assert!(cp.chain.len() >= 2, "path has at least 2 nodes");
        assert!(cp.total_weight > 0);
    }
}
