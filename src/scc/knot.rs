//! Knot detection — filter sink SCCs by business rules (§3.5).
//!
//! A "Knot" is a meaningful deadlock/bottleneck: a sink SCC in the
//! condensation DAG that passes business rule filtering.

use petgraph::graph::NodeIndex;

use crate::graph::types::*;
use crate::graph::wfg::WaitForGraph;

use super::tarjan::CondensationDag;

/// A detected Knot — a sink SCC that passes business filters.
#[derive(Debug, Clone)]
pub struct Knot {
    pub dag_index: NodeIndex,
    pub members: Vec<ThreadId>,
}

/// Detect Knots: sink SCCs that pass business rule filtering.
///
/// Business rules (§3.5):
/// 1. Exclude |SCC| == 1 with no self-loop (trivial sinks)
/// 2. Exclude pure kernel-thread SCCs (kworker, ksoftirqd, etc.)
/// 3. Remaining sinks with at least 1 userspace thread are Knots
pub fn detect_knots(
    cdag: &CondensationDag,
    graph: &WaitForGraph,
) -> Vec<Knot> {
    cdag.sinks()
        .into_iter()
        .filter(|&idx| {
            let sn = cdag.super_node(idx);

            // Rule 1: exclude trivial singletons (no self-loop)
            if sn.members.len() == 1 && !has_self_loop(graph, &sn.members[0]) {
                return false;
            }

            // Rule 2: exclude pure kernel-thread SCCs
            if is_pure_kernel_scc(graph, &sn.members) {
                return false;
            }

            true
        })
        .map(|idx| {
            let sn = cdag.super_node(idx);
            Knot {
                dag_index: idx,
                members: sn.members.clone(),
            }
        })
        .collect()
}

/// Check if a thread has a self-loop in the WFG.
fn has_self_loop(graph: &WaitForGraph, tid: &ThreadId) -> bool {
    graph
        .all_edges()
        .iter()
        .any(|(_, src, dst, _)| src == tid && dst == tid)
}

/// Check if ALL members of an SCC are kernel threads.
fn is_pure_kernel_scc(graph: &WaitForGraph, members: &[ThreadId]) -> bool {
    members.iter().all(|tid| {
        let idx = graph.node_index(tid).unwrap();
        matches!(
            graph.node_weight(idx).kind,
            NodeKind::KernelThread
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scc::tarjan::build_condensation;

    #[test]
    fn trivial_sink_excluded() {
        // A→B, B is a leaf (singleton sink, no self-loop) → not a knot
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));

        let cdag = build_condensation(&g);
        let knots = detect_knots(&cdag, &g);
        assert!(knots.is_empty(), "trivial singleton sink is not a knot");
    }

    #[test]
    fn self_loop_sink_is_knot() {
        // A→B, B→B (self-loop) → B is a knot
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(2), TimeWindow::new(0, 100));

        let cdag = build_condensation(&g);
        let knots = detect_knots(&cdag, &g);
        assert_eq!(knots.len(), 1);
        assert!(knots[0].members.contains(&ThreadId(2)));
    }

    #[test]
    fn cycle_sink_is_knot() {
        // A→B→C→B (B,C form a cycle, which is a sink SCC)
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(3), ThreadId(2), TimeWindow::new(0, 100));

        let cdag = build_condensation(&g);
        let knots = detect_knots(&cdag, &g);
        assert_eq!(knots.len(), 1);
        assert_eq!(knots[0].members.len(), 2);
    }

    #[test]
    fn pure_kernel_scc_excluded() {
        // Two kernel threads in a cycle → excluded
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(10), NodeKind::KernelThread);
        g.add_node(ThreadId(11), NodeKind::KernelThread);
        g.add_edge(ThreadId(1), ThreadId(10), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(10), ThreadId(11), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(11), ThreadId(10), TimeWindow::new(0, 100));

        let cdag = build_condensation(&g);
        let knots = detect_knots(&cdag, &g);
        assert!(knots.is_empty(), "pure kernel SCCs are excluded");
    }

    #[test]
    fn mixed_kernel_user_scc_is_knot() {
        // One user + one kernel thread in a cycle → knot (has user thread)
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(10), NodeKind::KernelThread);
        g.add_edge(ThreadId(1), ThreadId(10), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(10), ThreadId(1), TimeWindow::new(0, 100));

        let cdag = build_condensation(&g);
        let knots = detect_knots(&cdag, &g);
        assert_eq!(knots.len(), 1, "mixed kernel+user SCC is a knot");
    }

    #[test]
    fn non_sink_cycle_not_a_knot() {
        // A→B→A (cycle, but A→C so it's not a sink)
        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(1), NodeKind::UserThread);
        g.add_node(ThreadId(2), NodeKind::UserThread);
        g.add_node(ThreadId(3), NodeKind::UserThread);
        g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(2), ThreadId(1), TimeWindow::new(0, 100));
        g.add_edge(ThreadId(1), ThreadId(3), TimeWindow::new(0, 50));

        let cdag = build_condensation(&g);
        let knots = detect_knots(&cdag, &g);
        // {A,B} has outgoing edge to {C} → not a sink → not a knot
        // {C} is a trivial singleton → not a knot
        assert!(knots.is_empty());
    }
}
