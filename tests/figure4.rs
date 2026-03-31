//! Integration tests for OSDI'18 wPerf paper Figure 4 scenario.
//!
//! These are the fundamental correctness tests — if these fail,
//! the cascade engine is broken.

use wperf::cascade::engine::cascade_engine;
use wperf::cascade::invariants::is_conserved;
use wperf::graph::types::*;
use wperf::graph::wfg::WaitForGraph;
use wperf::output::CascadeResult;

/// Build the exact Figure 4 graph from the paper.
/// User(T1) waits 100ms for Parser(T2), Parser waits 80ms for Network(T3).
fn figure4_graph() -> WaitForGraph {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread); // User
    g.add_node(ThreadId(2), NodeKind::UserThread); // Parser
    g.add_node(ThreadId(3), NodeKind::PseudoNic); // Network
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));
    g
}

#[test]
fn figure4_attributed_values() {
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

    // Paper says: Parser directly responsible for 20ms, Network for 80ms
    assert_eq!(
        user_parser.3.attributed_delay_ms, 20,
        "User→Parser attributed"
    );
    assert_eq!(
        parser_network.3.attributed_delay_ms, 80,
        "Parser→Network attributed"
    );
}

#[test]
fn figure4_root_edge_conservation() {
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

    // Root edge raw (100ms) = sum of all attributed in the cascade chain
    let total_chain_attributed =
        user_parser.3.attributed_delay_ms + parser_network.3.attributed_delay_ms;
    assert_eq!(
        total_chain_attributed, 100,
        "cascade chain must account for full root wait"
    );
}

#[test]
fn figure4_invariants_hold() {
    let g = figure4_graph();
    let result = cascade_engine(&g, None).unwrap();
    assert!(is_conserved(&result), "I-1 sentinel must pass");
}

#[test]
fn figure4_json_output() {
    let g = figure4_graph();
    let result = cascade_engine(&g, None).unwrap();
    let output = CascadeResult::from_graph(&g, &result);

    assert!(output.graph_metrics.is_conserved);
    assert_eq!(output.graph_metrics.edge_count, 2);
    assert_eq!(output.graph_metrics.node_count, 3);

    // Verify JSON roundtrip
    let json = serde_json::to_string(&output).unwrap();
    assert!(json.contains("\"is_conserved\":true"));
}

/// Extended 3-node chain: User→Parser→Network→Disk.
#[test]
fn extended_chain_attributed_values() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread); // User
    g.add_node(ThreadId(2), NodeKind::UserThread); // Parser
    g.add_node(ThreadId(3), NodeKind::PseudoNic); // Network
    g.add_node(ThreadId(4), NodeKind::PseudoDisk); // Disk
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));
    g.add_edge(ThreadId(3), ThreadId(4), TimeWindow::new(50, 100));

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

    // User→Parser: raw=100, Parser waits 80ms for Network → attributed=20
    assert_eq!(up.3.attributed_delay_ms, 20, "User→Parser");
    // Parser→Network: raw=80, Network waits 50ms for Disk → attributed=30
    assert_eq!(pn.3.attributed_delay_ms, 30, "Parser→Network");
    // Network→Disk: raw=50, Disk is leaf → attributed=50
    assert_eq!(nd.3.attributed_delay_ms, 50, "Network→Disk");

    // Root edge fully distributed: 20 + 30 + 50 = 100
    let total = up.3.attributed_delay_ms + pn.3.attributed_delay_ms + nd.3.attributed_delay_ms;
    assert_eq!(total, 100);

    assert!(is_conserved(&result));
}

/// Single edge: leaf node gets full attribution.
#[test]
fn single_edge_full_attribution() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

    let result = cascade_engine(&g, None).unwrap();
    let edges = result.all_edges();
    assert_eq!(edges[0].3.attributed_delay_ms, 50);
    assert!(is_conserved(&result));
}

/// Disconnected components: each edge is independent.
#[test]
fn disconnected_edges() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_node(ThreadId(3), NodeKind::UserThread);
    g.add_node(ThreadId(4), NodeKind::UserThread);
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));
    g.add_edge(ThreadId(3), ThreadId(4), TimeWindow::new(0, 80));

    let result = cascade_engine(&g, None).unwrap();
    let edges = result.all_edges();

    let e1 = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(1) && *d == ThreadId(2))
        .unwrap();
    let e2 = edges
        .iter()
        .find(|(_, s, d, _)| *s == ThreadId(3) && *d == ThreadId(4))
        .unwrap();

    // Both are leaf edges — full attribution
    assert_eq!(e1.3.attributed_delay_ms, 50);
    assert_eq!(e2.3.attributed_delay_ms, 80);
    assert!(is_conserved(&result));
}
