//! Output types for cascade results.

use serde::Serialize;

use crate::cascade::invariants;
use crate::graph::types::ThreadId;
use crate::graph::wfg::WaitForGraph;

#[derive(Debug, Serialize)]
pub struct CascadeResult {
    pub edges: Vec<EdgeOutput>,
    pub graph_metrics: GraphMetrics,
}

#[derive(Debug, Serialize)]
pub struct EdgeOutput {
    pub src: ThreadId,
    pub dst: ThreadId,
    pub raw_wait_ms: u64,
    pub attributed_delay_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct GraphMetrics {
    pub total_raw_wait_ms: u64,
    pub total_attributed_delay_ms: u64,
    pub is_conserved: bool,
    pub edge_count: usize,
    pub node_count: usize,
}

impl CascadeResult {
    pub fn from_graph(original: &WaitForGraph, result: &WaitForGraph) -> Self {
        let edges: Vec<EdgeOutput> = result
            .all_edges()
            .iter()
            .map(|(_, src, dst, ew)| EdgeOutput {
                src: *src,
                dst: *dst,
                raw_wait_ms: ew.raw_wait_ms,
                attributed_delay_ms: ew.attributed_delay_ms,
            })
            .collect();

        let total_raw = original.total_raw_wait();
        let total_attr = result.total_attributed();

        Self {
            edges,
            graph_metrics: GraphMetrics {
                total_raw_wait_ms: total_raw,
                total_attributed_delay_ms: total_attr,
                is_conserved: invariants::is_conserved(result),
                edge_count: result.edge_count(),
                node_count: result.node_count(),
            },
        }
    }
}
