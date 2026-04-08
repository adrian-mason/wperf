//! Graphviz DOT and SVG backends for cascade results.
//!
//! Converts a `CascadeResult` into DOT format for visualization, and
//! optionally renders SVG by invoking external Graphviz `dot -Tsvg`.

use std::fmt::Write;
use std::process::Command;

use crate::graph::types::ThreadId;
use crate::output::{CascadeResult, EdgeOutput};
use crate::report::ReportError;

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
    let mut nodes: Vec<i64> = cascade
        .edges
        .iter()
        .flat_map(|e| [e.src.0, e.dst.0])
        .collect();
    nodes.sort_unstable();
    nodes.dedup();

    // Emit nodes. Labels use ThreadId::Display for human-readable names
    // (e.g. "NIC", "Disk" for pseudo-threads, "T101" for regular threads).
    for tid in &nodes {
        writeln!(out, "    {} [label=\"{}\"];", dot_id(*tid), ThreadId(*tid)).unwrap();
    }

    // Emit edges sorted by full key for determinism — includes label fields
    // so output is self-contained and doesn't depend on upstream edge order.
    let mut edges: Vec<&EdgeOutput> = cascade.edges.iter().collect();
    edges.sort_unstable_by_key(|e| (e.src, e.dst, e.attributed_delay_ms, e.raw_wait_ms));

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

/// Render a `CascadeResult` as SVG by piping DOT through Graphviz `dot -Tsvg`.
///
/// Returns `ReportError::GraphvizNotFound` if `dot` is not in PATH, or
/// `ReportError::GraphvizFailed` if the process exits non-zero.
pub fn render_svg(cascade: &CascadeResult) -> Result<Vec<u8>, ReportError> {
    render_svg_with_command(cascade, "dot")
}

/// Internal testable seam: same as `render_svg` but accepts a custom command path.
fn render_svg_with_command(
    cascade: &CascadeResult,
    dot_command: &str,
) -> Result<Vec<u8>, ReportError> {
    let dot_input = render_dot(cascade);

    let mut child = Command::new(dot_command)
        .args(["-Tsvg"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ReportError::GraphvizNotFound
            } else {
                ReportError::Io(e)
            }
        })?;

    // Write DOT to stdin in a separate thread to avoid deadlock: if dot's
    // stdout/stderr buffers fill before we finish writing, both processes block.
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let writer_thread = std::thread::spawn(move || {
        use std::io::Write;
        stdin.write_all(dot_input.as_bytes())
    });

    let output = child.wait_with_output().map_err(ReportError::Io)?;

    // Propagate any stdin write error.
    writer_thread
        .join()
        .expect("stdin writer thread panicked")
        .map_err(ReportError::Io)?;

    if !output.status.success() {
        return Err(ReportError::GraphvizFailed {
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::ThreadId;
    use crate::output::{CascadeResult, EdgeOutput, GraphMetrics};
    use crate::report::ReportError;
    use std::process::Command;

    fn make_cascade(edges: Vec<EdgeOutput>) -> CascadeResult {
        let edge_count = edges.len();
        let mut node_ids: Vec<i64> = edges.iter().flat_map(|e| [e.src.0, e.dst.0]).collect();
        node_ids.sort_unstable();
        node_ids.dedup();
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
        assert!(dot.contains("neg_4 [label=\"NIC\"]"));
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

    #[test]
    fn duplicate_src_dst_deterministic() {
        // Two edges with same (src, dst) but different weights must have
        // deterministic ordering based on label fields.
        let cascade = make_cascade(vec![
            EdgeOutput {
                src: ThreadId(100),
                dst: ThreadId(200),
                raw_wait_ms: 10,
                attributed_delay_ms: 8,
            },
            EdgeOutput {
                src: ThreadId(100),
                dst: ThreadId(200),
                raw_wait_ms: 5,
                attributed_delay_ms: 3,
            },
        ]);
        let dot = render_dot(&cascade);

        // Smaller attributed_delay_ms (3) must appear before larger (8).
        let pos_3ms = dot.find("label=\"3ms (raw 5ms)\"").unwrap();
        let pos_8ms = dot.find("label=\"8ms (raw 10ms)\"").unwrap();
        assert!(pos_3ms < pos_8ms);
    }

    // --- SVG rendering tests ---

    #[test]
    fn svg_missing_command_returns_not_found() {
        let cascade = make_cascade(vec![]);
        let err = render_svg_with_command(&cascade, "nonexistent-dot-binary-xyz")
            .expect_err("should fail with missing command");
        assert!(
            matches!(err, ReportError::GraphvizNotFound),
            "expected GraphvizNotFound, got: {err}"
        );
    }

    #[test]
    fn svg_bad_command_returns_failed() {
        // Use `false` as the command — it exits with code 1.
        let cascade = make_cascade(vec![]);
        let err =
            render_svg_with_command(&cascade, "false").expect_err("should fail with non-zero exit");
        assert!(
            matches!(err, ReportError::GraphvizFailed { .. }),
            "expected GraphvizFailed, got: {err}"
        );
    }

    #[test]
    fn svg_single_edge_produces_valid_svg() {
        // Skip if `dot` is not installed (developer convenience).
        if Command::new("dot").arg("-V").output().is_err() {
            eprintln!("skipping svg test: dot not found");
            return;
        }
        let cascade = make_cascade(vec![EdgeOutput {
            src: ThreadId(100),
            dst: ThreadId(200),
            raw_wait_ms: 5,
            attributed_delay_ms: 3,
        }]);
        let svg = render_svg_with_command(&cascade, "dot").unwrap();
        let svg_str = String::from_utf8(svg).expect("SVG should be valid UTF-8");
        assert!(
            svg_str.contains("<svg"),
            "output should contain <svg element"
        );
        assert!(
            svg_str.contains("</svg>"),
            "output should contain closing </svg>"
        );
    }

    #[test]
    fn svg_empty_graph_produces_valid_svg() {
        if Command::new("dot").arg("-V").output().is_err() {
            eprintln!("skipping svg test: dot not found");
            return;
        }
        let cascade = make_cascade(vec![]);
        let svg = render_svg_with_command(&cascade, "dot").unwrap();
        let svg_str = String::from_utf8(svg).expect("SVG should be valid UTF-8");
        assert!(svg_str.contains("<svg"));
        assert!(svg_str.contains("</svg>"));
    }
}
