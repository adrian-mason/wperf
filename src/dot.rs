//! Graphviz DOT backend for cascade results.
//!
//! Converts a `CascadeResult` into DOT format for visualization via
//! `dot -Tsvg`. This is the W3 #19 DOT text backend; actual SVG
//! generation is deferred to a follow-up task.

use std::fmt::Write;

use crate::output::{CascadeResult, EdgeOutput};

/// Render a `CascadeResult` as a Graphviz DOT digraph.
///
/// Output is deterministic: nodes sorted by `ThreadId`, edges sorted by
/// (src, dst). All identifiers are escaped for DOT safety.
pub fn render_dot(cascade: &CascadeResult) -> String {
    let mut out = String::new();
    writeln!(out, "digraph wperf {{").unwrap();
    writeln!(out, "    rankdir=LR;").unwrap();
    writeln!(out, "    node [shape=box];").unwrap();

    // Collect and sort unique node ids for deterministic output.
    let mut nodes: Vec<i64> = Vec::new();
    for edge in &cascade.edges {
        if !nodes.contains(&edge.src.0) {
            nodes.push(edge.src.0);
        }
        if !nodes.contains(&edge.dst.0) {
            nodes.push(edge.dst.0);
        }
    }
    nodes.sort_unstable();

    // Emit nodes.
    for tid in &nodes {
        writeln!(
            out,
            "    {} [label=\"T{}\"];",
            dot_id(*tid),
            dot_escape_label(*tid)
        )
        .unwrap();
    }

    // Emit edges sorted by (src, dst) for determinism.
    let mut edges: Vec<&EdgeOutput> = cascade.edges.iter().collect();
    edges.sort_unstable_by_key(|e| (e.src, e.dst));

    for edge in edges {
        writeln!(
            out,
            "    {} -> {} [label=\"{}ms (raw {}ms)\"];",
            dot_id(edge.src.0),
            dot_id(edge.dst.0),
            edge.attributed_delay_ms,
            edge.raw_wait_ms,
        )
        .unwrap();
    }

    writeln!(out, "}}").unwrap();
    out
}

/// Produce a DOT-safe node identifier from a thread id.
///
/// Negative ids (pseudo-threads) get a `neg_` prefix to avoid DOT syntax
/// issues with leading `-`.
fn dot_id(tid: i64) -> String {
    if tid < 0 {
        format!("neg_{}", tid.unsigned_abs())
    } else {
        format!("t{tid}")
    }
}

/// Produce a DOT-safe label string for a thread id.
fn dot_escape_label(tid: i64) -> String {
    format!("{tid}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::ThreadId;
    use crate::output::{CascadeResult, EdgeOutput, GraphMetrics};

    fn make_cascade(edges: Vec<EdgeOutput>) -> CascadeResult {
        let edge_count = edges.len();
        let mut node_ids: Vec<i64> = Vec::new();
        for e in &edges {
            if !node_ids.contains(&e.src.0) {
                node_ids.push(e.src.0);
            }
            if !node_ids.contains(&e.dst.0) {
                node_ids.push(e.dst.0);
            }
        }
        CascadeResult {
            edges,
            graph_metrics: GraphMetrics {
                total_raw_wait_ms: 0,
                total_attributed_delay_ms: 0,
                invariants_ok: true,
                edge_count,
                node_count: node_ids.len(),
            },
        }
    }

    #[test]
    fn empty_graph() {
        let cascade = make_cascade(vec![]);
        let dot = render_dot(&cascade);
        assert!(dot.contains("digraph wperf {"));
        assert!(dot.contains('}'));
        // No nodes or edges
        assert!(!dot.contains("->"));
        assert!(!dot.contains("[label=\"T"));
    }

    #[test]
    fn single_edge() {
        let cascade = make_cascade(vec![EdgeOutput {
            src: ThreadId(100),
            dst: ThreadId(200),
            raw_wait_ms: 5,
            attributed_delay_ms: 3,
        }]);
        let dot = render_dot(&cascade);
        assert!(dot.contains("t100 [label=\"T100\"]"));
        assert!(dot.contains("t200 [label=\"T200\"]"));
        assert!(dot.contains("t100 -> t200 [label=\"3ms (raw 5ms)\"]"));
    }

    #[test]
    fn negative_tid_escaping() {
        let cascade = make_cascade(vec![EdgeOutput {
            src: ThreadId(-4),
            dst: ThreadId(100),
            raw_wait_ms: 10,
            attributed_delay_ms: 8,
        }]);
        let dot = render_dot(&cascade);
        assert!(dot.contains("neg_4 [label=\"T-4\"]"));
        assert!(dot.contains("neg_4 -> t100"));
    }

    #[test]
    fn deterministic_output() {
        // Create edges in non-sorted order, verify output is sorted.
        let cascade = make_cascade(vec![
            EdgeOutput {
                src: ThreadId(300),
                dst: ThreadId(100),
                raw_wait_ms: 2,
                attributed_delay_ms: 1,
            },
            EdgeOutput {
                src: ThreadId(100),
                dst: ThreadId(200),
                raw_wait_ms: 5,
                attributed_delay_ms: 3,
            },
        ]);
        let dot1 = render_dot(&cascade);
        let dot2 = render_dot(&cascade);
        assert_eq!(dot1, dot2);

        // Nodes should appear in sorted order: 100, 200, 300
        let pos_100 = dot1.find("t100 [label").unwrap();
        let pos_200 = dot1.find("t200 [label").unwrap();
        let pos_300 = dot1.find("t300 [label").unwrap();
        assert!(pos_100 < pos_200);
        assert!(pos_200 < pos_300);

        // Edges should appear sorted by (src, dst): 100→200 before 300→100
        let edge_100_200 = dot1.find("t100 -> t200").unwrap();
        let edge_300_100 = dot1.find("t300 -> t100").unwrap();
        assert!(edge_100_200 < edge_300_100);
    }
}
