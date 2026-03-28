//! Differential testing: Rust vs Python oracle.
//!
//! Runs the same synthetic graphs through both implementations and
//! verifies per-edge attributed_delay agrees within 1.0ms tolerance.

use std::process::Command;

use serde::{Deserialize, Serialize};

use wperf::cascade::engine::cascade_engine;
use wperf::graph::types::*;
use wperf::graph::wfg::WaitForGraph;

const TOLERANCE_MS: u64 = 1;

#[derive(Serialize)]
struct OracleInput {
    nodes: Vec<OracleNode>,
    edges: Vec<OracleEdge>,
}

#[derive(Serialize)]
struct OracleNode {
    tid: i64,
    kind: String,
}

#[derive(Serialize)]
struct OracleEdge {
    src: i64,
    dst: i64,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Deserialize)]
struct OracleOutput {
    edges: Vec<OracleResultEdge>,
}

#[derive(Deserialize)]
struct OracleResultEdge {
    src: i64,
    dst: i64,
    raw_wait_ms: u64,
    attributed_delay_ms: u64,
}

fn graph_to_oracle_input(g: &WaitForGraph) -> OracleInput {
    let nodes: Vec<OracleNode> = g
        .node_indices()
        .iter()
        .map(|&idx| {
            let nw = g.node_weight(idx);
            OracleNode {
                tid: nw.tid.0,
                kind: format!("{:?}", nw.kind),
            }
        })
        .collect();

    let edges: Vec<OracleEdge> = g
        .all_edges()
        .iter()
        .map(|(_, src, dst, ew)| OracleEdge {
            src: src.0,
            dst: dst.0,
            start_ms: ew.time_window.start_ms,
            end_ms: ew.time_window.end_ms,
        })
        .collect();

    OracleInput { nodes, edges }
}

fn run_python_oracle(input: &OracleInput) -> OracleOutput {
    let json_input = serde_json::to_string(input).unwrap();

    // Find the oracle script relative to the test binary
    let oracle_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("cascade_oracle.py");

    let output = Command::new("python3")
        .arg(&oracle_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(json_input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .expect("failed to run Python oracle");

    assert!(
        output.status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("failed to parse Python output")
}

fn compare_results(rust_graph: &WaitForGraph, python: &OracleOutput, test_name: &str) {
    let rust_edges = rust_graph.all_edges();
    assert_eq!(
        rust_edges.len(),
        python.edges.len(),
        "{}: edge count mismatch",
        test_name
    );

    for (i, (_, src, dst, ew)) in rust_edges.iter().enumerate() {
        let py = &python.edges[i];
        assert_eq!(src.0, py.src, "{}: edge {} src mismatch", test_name, i);
        assert_eq!(dst.0, py.dst, "{}: edge {} dst mismatch", test_name, i);
        assert_eq!(
            ew.raw_wait_ms, py.raw_wait_ms,
            "{}: edge {} raw_wait mismatch",
            test_name, i
        );

        let diff = if ew.attributed_delay_ms > py.attributed_delay_ms {
            ew.attributed_delay_ms - py.attributed_delay_ms
        } else {
            py.attributed_delay_ms - ew.attributed_delay_ms
        };

        assert!(
            diff <= TOLERANCE_MS,
            "{}: edge {} ({}->{}) attributed mismatch: rust={}, python={}, diff={}",
            test_name,
            i,
            src,
            dst,
            ew.attributed_delay_ms,
            py.attributed_delay_ms,
            diff
        );
    }
}

fn run_differential(g: &WaitForGraph, test_name: &str) {
    let rust_result = cascade_engine(g, None);
    let input = graph_to_oracle_input(g);
    let python_result = run_python_oracle(&input);
    compare_results(&rust_result, &python_result, test_name);
}

#[test]
fn figure4_agreement() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_node(ThreadId(3), NodeKind::PseudoNic);
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));

    run_differential(&g, "figure4");
}

#[test]
fn extended_chain_agreement() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_node(ThreadId(3), NodeKind::PseudoNic);
    g.add_node(ThreadId(4), NodeKind::PseudoDisk);
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 100));
    g.add_edge(ThreadId(3), ThreadId(4), TimeWindow::new(50, 100));

    run_differential(&g, "extended_chain");
}

#[test]
fn single_edge_agreement() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 50));

    run_differential(&g, "single_edge");
}

#[test]
fn overlapping_edges_agreement() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_node(ThreadId(3), NodeKind::UserThread);
    g.add_node(ThreadId(4), NodeKind::UserThread);
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 60));
    g.add_edge(ThreadId(2), ThreadId(4), TimeWindow::new(20, 80));

    run_differential(&g, "overlapping_edges");
}

#[test]
fn diamond_agreement() {
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_node(ThreadId(3), NodeKind::UserThread);
    g.add_node(ThreadId(4), NodeKind::UserThread);
    g.add_node(ThreadId(5), NodeKind::UserThread);
    g.add_edge(ThreadId(1), ThreadId(2), TimeWindow::new(0, 100));
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(0, 50));
    g.add_edge(ThreadId(2), ThreadId(4), TimeWindow::new(50, 100));
    g.add_edge(ThreadId(3), ThreadId(5), TimeWindow::new(0, 50));
    g.add_edge(ThreadId(4), ThreadId(5), TimeWindow::new(50, 100));

    run_differential(&g, "diamond");
}

#[test]
fn concurrent_waiters_agreement() {
    // Two threads waiting for the same target simultaneously
    let mut g = WaitForGraph::new();
    g.add_node(ThreadId(1), NodeKind::UserThread);
    g.add_node(ThreadId(2), NodeKind::UserThread);
    g.add_node(ThreadId(3), NodeKind::UserThread);
    g.add_edge(ThreadId(1), ThreadId(3), TimeWindow::new(0, 100));
    g.add_edge(ThreadId(2), ThreadId(3), TimeWindow::new(20, 80));

    run_differential(&g, "concurrent_waiters");
}
