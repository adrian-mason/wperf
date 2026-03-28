//! Regression tests for 5 known bugs found during pseudocode review.
//!
//! Each test constructs the minimal graph triggering the specific bug
//! and verifies the fix produces correct output.

use wperf::cascade::engine::{cascade_engine, is_conserved};
use wperf::graph::types::*;
use wperf::graph::wfg::WaitForGraph;

/// BUG-1: visited_path scope leak across DFS branches.
///
/// Bug mechanism: if the cycle-detection path set persists across
/// sibling branches in the DFS tree, a node reachable via two paths
/// is incorrectly skipped on the second visit. The fix uses
/// path.insert()/path.remove() so each branch gets a clean path.
///
/// Graph: A→B [0,100), B→C [0,50), B→D [50,100), C→E [0,50), D→E [50,100)
/// E is reachable from B via C and via D in different time windows.
#[test]
fn bug1_visited_path_scope() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread); // A
    g.add_node(ThreadId(2), NodeKind::UserThread); // B
    g.add_node(ThreadId(3), NodeKind::UserThread); // C
    g.add_node(ThreadId(4), NodeKind::UserThread); // D
    g.add_node(ThreadId(5), NodeKind::UserThread); // E

    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100)); // A→B
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 50)); // B→C
    g.add_edge(ThreadId(2), ThreadId(4), TimeWindow::new(50, 100)); // B→D
    g.add_edge(ThreadId(3), ThreadId(5), TimeWindow::new(0, 50)); // C→E
    g.add_edge(ThreadId(4), ThreadId(5), TimeWindow::new(50, 100)); // D→E

    let result = cascade_engine(&g, None);

    // A→B: B is busy for the full 100ms (with C then D) → attributed=0
    let edges = result.all_edges();
    let ab = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
        .unwrap();
    assert_eq!(ab.3.attributed_delay_ms, 0, "A→B should propagate everything");

    // Both C→E and D→E should have non-zero attribution (leaf paths)
    let ce = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(3) && *d == ThreadId(5))
        .unwrap();
    let de = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(4) && *d == ThreadId(5))
        .unwrap();
    assert!(
        ce.3.attributed_delay_ms > 0,
        "C→E must not be zero (BUG-1: E was skipped via C path)"
    );
    assert!(
        de.3.attributed_delay_ms > 0,
        "D→E must not be zero (BUG-1: E was skipped via D path)"
    );

    assert!(is_conserved(&g, &result));
}

/// BUG-2: propagated_down return value ignored.
///
/// Bug mechanism: if the parent does not subtract the weight
/// propagated to its children, it claims the full raw_wait
/// as its own attribution — inflating blame on intermediate nodes.
///
/// Graph: A→B [0,100), B→C [20,100)
/// Without fix: A→B.attributed = 100 (no subtraction)
/// With fix: A→B.attributed = 20 (80ms propagated to C)
#[test]
fn bug2_propagated_down_ignored() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread); // A
    g.add_node(ThreadId(2), NodeKind::UserThread); // B
    g.add_node(ThreadId(3), NodeKind::UserThread); // C

    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

    let result = cascade_engine(&g, None);
    let edges = result.all_edges();

    let ab = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
        .unwrap();

    // If BUG-2 exists, attributed = 100 (propagated ignored)
    assert_ne!(
        ab.3.attributed_delay_ms, 100,
        "BUG-2: propagated_down must be subtracted"
    );
    assert_eq!(ab.3.attributed_delay_ms, 20);

    assert!(is_conserved(&g, &result));
}

/// BUG-3: multi-edge overlap double-counting.
///
/// Bug mechanism: without sweep-line partition, overlapping outgoing
/// edges from the same node are processed independently, counting
/// the overlap region twice and over-propagating weight.
///
/// Graph: A→B [0,100), B→C [0,60), B→D [20,80)
/// Overlap region [20,60) has both C and D active.
/// Without sweep-line: propagated = 60 + 60 = 120 > raw=100 → attributed=0
/// With sweep-line: correct partitioning → attributed > 0
#[test]
fn bug3_multi_edge_overlap() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread); // A
    g.add_node(ThreadId(2), NodeKind::UserThread); // B
    g.add_node(ThreadId(3), NodeKind::UserThread); // C
    g.add_node(ThreadId(4), NodeKind::UserThread); // D

    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100)); // A→B
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 60)); // B→C
    g.add_edge(ThreadId(2), ThreadId(4), TimeWindow::new(20, 80)); // B→D

    let result = cascade_engine(&g, None);
    let edges = result.all_edges();

    let ab = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
        .unwrap();

    // With sweep-line: [0,20)={C}→20, [20,60)={C,D}→20+20, [60,80)={D}→20
    // Total propagated from B = 80, attributed = 100-80 = 20
    // Without sweep-line (BUG-3): propagated = 120, attributed = 0
    assert!(
        ab.3.attributed_delay_ms > 0,
        "BUG-3: sweep-line must prevent double-counting"
    );
    // B is idle during [80,100) → 20ms is B's direct fault
    assert_eq!(ab.3.attributed_delay_ms, 20);

    assert!(is_conserved(&g, &result));
}

/// BUG-4: BUG-2 + BUG-3 combined.
///
/// Bug mechanism: chain with overlapping edges at intermediate node
/// triggers both propagated_down ignored AND double-counting.
///
/// Graph: A→B [0,100), B→C [0,60), B→D [20,80), C→E [0,60)
/// Both bugs amplify the error at intermediate nodes.
#[test]
fn bug4_combined_bug2_bug3() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread); // A
    g.add_node(ThreadId(2), NodeKind::UserThread); // B
    g.add_node(ThreadId(3), NodeKind::UserThread); // C
    g.add_node(ThreadId(4), NodeKind::UserThread); // D
    g.add_node(ThreadId(5), NodeKind::UserThread); // E

    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100)); // A→B
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 60)); // B→C
    g.add_edge(ThreadId(2), ThreadId(4), TimeWindow::new(20, 80)); // B→D
    g.add_edge(ThreadId(3), ThreadId(5), TimeWindow::new(0, 60)); // C→E

    let result = cascade_engine(&g, None);
    let edges = result.all_edges();

    let ab = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
        .unwrap();
    let bc = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(2) && *d == ThreadId(3))
        .unwrap();

    // A→B.attributed should reflect correct cascade through B's children
    assert!(
        ab.3.attributed_delay_ms < ab.3.raw_wait_ms,
        "A→B must propagate some weight to children"
    );
    // B→C should propagate to E
    assert!(
        bc.3.attributed_delay_ms < bc.3.raw_wait_ms,
        "B→C must propagate weight to E"
    );

    assert!(is_conserved(&g, &result));
}

/// NEW-BUG-1: leaf node zero blame.
///
/// Bug mechanism: when a node has no outgoing edges (leaf),
/// compute_cascade returns 0 propagated. If the parent uses
/// only the propagated value (not interval duration) to determine
/// how much the child absorbs, the leaf gets zero credit and
/// the parent keeps all the blame.
///
/// Graph: A→B [0,50)
/// B is a leaf — it should get full attribution (50ms), not 0.
#[test]
fn new_bug1_leaf_node_zero_blame() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

    let result = cascade_engine(&g, None);
    let edges = result.all_edges();

    // B is a leaf — full attribution belongs to B
    assert_eq!(
        edges[0].3.attributed_delay_ms, 50,
        "NEW-BUG-1: leaf node must get full attribution"
    );

    assert!(is_conserved(&g, &result));
}
