//! `wperf report` — Phase 1 / W3 partial report backend.
//!
//! Orchestrates the full offline analysis pipeline:
//! parse + sort + correlate + cascade + SCC + heuristic + critical path + knots,
//! then serializes a JSON report to stdout.
//!
//! This is NOT the final §1.2 / §5.1 self-contained HTML report (Phase 3).
//! See plan v2 for the explicit JSON field inventory and deferred fields.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, Write};

use serde::Serialize;

use crate::cascade::engine::{self, InvariantError};
use crate::cli::{ReportArgs, ReportFormat};
use crate::critical_path::{self, CriticalPath};
use crate::dot;
use crate::format::reader::{ReaderError, WperfReader};
use crate::output::CascadeResult;
use crate::pipeline::{self, PipelineError, PipelineStats};
use crate::scc::heuristic::apply_max_heuristic;
use crate::scc::knot::{self, Knot};
use crate::scc::tarjan::build_condensation;

/// Report-level error.
#[derive(Debug)]
pub enum ReportError {
    Io(std::io::Error),
    Pipeline(PipelineError),
    Cascade(InvariantError),
    /// Graphviz `dot` executable not found in PATH.
    GraphvizNotFound,
    /// Graphviz `dot` exited with a non-zero status.
    GraphvizFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Pipeline(e) => write!(f, "pipeline error: {e}"),
            Self::Cascade(e) => write!(f, "cascade invariant error: {e}"),
            Self::GraphvizNotFound => write!(
                f,
                "Graphviz 'dot' not found in PATH; \
                 install graphviz (https://graphviz.org) or use --format dot"
            ),
            Self::GraphvizFailed { exit_code, stderr } => {
                write!(f, "Graphviz 'dot' failed")?;
                if let Some(code) = exit_code {
                    write!(f, " (exit code {code})")?;
                }
                if !stderr.is_empty() {
                    // Bound stderr to avoid dumping unbounded process output.
                    let truncated: String = stderr.chars().take(1024).collect();
                    write!(f, ": {truncated}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ReportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Pipeline(e) => Some(e),
            Self::Cascade(e) => Some(e),
            Self::GraphvizNotFound | Self::GraphvizFailed { .. } => None,
        }
    }
}

impl From<std::io::Error> for ReportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<PipelineError> for ReportError {
    fn from(e: PipelineError) -> Self {
        Self::Pipeline(e)
    }
}

impl From<InvariantError> for ReportError {
    fn from(e: InvariantError) -> Self {
        Self::Cascade(e)
    }
}

/// Top-level JSON report output.
#[derive(Debug, Serialize)]
pub struct ReportOutput {
    pub cascade: CascadeResult,
    pub critical_path: Option<CriticalPath>,
    pub knots: Vec<Knot>,
    pub stats: PipelineStats,
    pub health: HealthMetrics,
}

/// Coverage and health metrics (§5.5).
///
/// Actual metrics are wired from the pipeline; fields that lack plumbing
/// in Phase 1 are `None` (serialized as `null`, meaning "not yet measured").
#[derive(Debug, Serialize)]
pub struct HealthMetrics {
    /// Structural postcondition guard (I-2 ∧ I-7). `false` means cascade
    /// engine violation — results should not be trusted.
    pub invariants_ok: bool,
    /// BPF-side event drops (ringbuf reserve failures / perfarray overflows).
    pub drop_count: Option<u64>,
    /// Wakeup events with no matching off-CPU switch — measures correlation
    /// completeness.
    pub unmatched_wakeup_count: u64,
    /// Unavailable: no stack capture in Phase 1.
    pub partial_stack_count: Option<u64>,
    /// Unavailable: cascade engine depth limit exists but is not instrumented.
    pub cascade_depth_truncation_count: Option<u64>,
    /// Wakeup edges pruned by the spurious wakeup filter (§2.5).
    pub false_wakeup_filtered_count: Option<u64>,

    // --- Block-IO attribution health (Phase 2b #38 commit-5) ---------------
    /// `IoComplete` events with no matching `IoIssue` in the userspace
    /// `pending_io` map. Populated if block-IO tracing ran, else `None`.
    pub io_orphan_complete_count: Option<u64>,
    /// `IoIssue` records still pending when correlation ended (no matching
    /// `IoComplete` arrived before capture stopped). Populated if block-IO
    /// tracing ran, else `None`.
    pub io_pending_at_end_count: Option<u64>,
    /// `IoIssue` events that overwrote an existing `pending_io` entry due
    /// to a `(dev, sector, nr_sector)` key collision. Guardrail for the
    /// collision window Gemini flagged on PR #120 — if this fires under
    /// real workloads, the event ABI needs to carry `struct request *`.
    pub io_userspace_pair_collision_count: Option<u64>,
    /// Fraction of block-IO raw wait time that flowed through the cascade
    /// to become attributed delay on `WaitType::IoBlock` edges. Phase 2b
    /// gate: `≥ 0.70` (final-design.md §3.8). `None` when no `IoBlock`
    /// edges exist (tracing disabled or no I/O observed) or when total
    /// `raw_wait` is zero (all sub-ms I/Os — ratio is undefined).
    pub attributed_delay_ratio: Option<f64>,
}

/// CLI entry point: opens the file, runs the pipeline, writes output to stdout.
pub fn run(args: &ReportArgs) -> Result<(), ReportError> {
    let file = File::open(&args.input)?;
    let mut reader = WperfReader::open(file).map_err(|e| match e {
        ReaderError::Io(io) => ReportError::Io(io),
        other => ReportError::Pipeline(PipelineError::Reader(other)),
    })?;
    let threshold_ns = u64::from(args.spurious_threshold_us) * 1_000;
    let report = build_report(&mut reader, threshold_ns)?;

    match args.format {
        ReportFormat::Json => {
            let stdout = std::io::stdout().lock();
            let mut writer = BufWriter::new(stdout);
            serde_json::to_writer_pretty(&mut writer, &report)
                .map_err(|e| ReportError::Io(e.into()))?;
            writer.flush()?;
        }
        ReportFormat::Dot => {
            let stdout = std::io::stdout().lock();
            let mut writer = BufWriter::new(stdout);
            let dot_output = dot::render_dot(&report.cascade);
            writer.write_all(dot_output.as_bytes())?;
            writer.flush()?;
        }
        ReportFormat::Svg => {
            let svg = dot::render_svg(&report.cascade)?;
            let stdout = std::io::stdout().lock();
            let mut writer = BufWriter::new(stdout);
            writer.write_all(&svg)?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Pure, testable report builder: runs the full analysis pipeline on a reader.
pub fn build_report<R: Read + Seek>(
    reader: &mut WperfReader<R>,
    spurious_threshold_ns: u64,
) -> Result<ReportOutput, ReportError> {
    // Step 1-3: parse + sort + correlate + noise edge pruning (§2.5)
    let (wfg, stats) = pipeline::build_wait_for_graph(reader, spurious_threshold_ns)?;

    // Read footer metadata for drop_count — propagate real errors (malformed
    // footer, oversized payload, I/O). Crash-recovery (no footer) is already
    // expressed as Ok(Metadata { drop_count: None, .. }) by the reader.
    let metadata = reader.read_metadata().map_err(PipelineError::Reader)?;
    let drop_count = metadata.drop_count;

    // Step 4: cascade redistribution
    let cascaded = engine::cascade_engine(&wfg, None)?;
    let cascade = CascadeResult::from_graph(&wfg, &cascaded);

    // Step 5-6: SCC condensation + MAX heuristic
    let mut cdag = build_condensation(&cascaded);
    apply_max_heuristic(&mut cdag, &cascaded);

    // Step 7: critical path DP
    let critical_path = critical_path::critical_path_dp(&cdag);

    // Knot detection
    let knots = knot::detect_knots(&cdag, &cascaded);

    let invariants_ok = cascade.graph_metrics.invariants_ok;
    let unmatched_wakeup_count = stats.correlation.unmatched_wakeup_count;
    let false_wakeup_filtered_count = stats.correlation.false_wakeup_filtered_count;

    let io_orphan_complete_count = Some(stats.correlation.io_orphan_complete_count);
    let io_pending_at_end_count = Some(stats.correlation.io_pending_at_end_count);
    let io_userspace_pair_collision_count =
        Some(stats.correlation.io_userspace_pair_collision_count);
    let attributed_delay_ratio = compute_io_attributed_delay_ratio(&cascaded);

    Ok(ReportOutput {
        cascade,
        critical_path,
        knots,
        stats,
        health: HealthMetrics {
            invariants_ok,
            drop_count,
            unmatched_wakeup_count,
            partial_stack_count: None,
            cascade_depth_truncation_count: None,
            false_wakeup_filtered_count: Some(false_wakeup_filtered_count),
            io_orphan_complete_count,
            io_pending_at_end_count,
            io_userspace_pair_collision_count,
            attributed_delay_ratio,
        },
    })
}

/// Compute the Phase 2b `attributed_delay_ratio` per the formal §7.3
/// definition (final-design.md, post-PR-#121 amendment):
///
/// `ratio = Σ(e.attributed_delay_ms) / Σ(e.raw_wait_ms)` for every edge
/// `e` where `e.dst` is an IO pseudo-thread AND
/// `e.kind != EdgeKind::SyntheticClosureReturn`.
///
/// The aggregate sums over all IO pseudo-thread destinations (currently
/// only `PseudoDisk`; Phase 2c adds `PseudoNic` / `PseudoSoftirq`). This
/// matches the gate's measurement domain — only forward edges entering
/// pseudo-threads carry real I/O service-time blame; closure-return
/// edges are SCC bookkeeping per ADR-009 *Amendment 2026-04-25* and
/// have no semantic wait time.
///
/// Returns `None` when no qualifying edge exists (no observable IO in
/// this workload) or when total `raw_wait` sums to zero (all sub-ms
/// I/Os collapsed to zero-duration windows). Per §7.3 the Phase 2b
/// gate treats `None` as a hard precondition failure (workload didn't
/// exercise the IO Attribution gate's subject); CI tri-state mapping
/// is applied at the test-runner level.
fn compute_io_attributed_delay_ratio(cascaded: &crate::graph::wfg::WaitForGraph) -> Option<f64> {
    use crate::graph::types::{EdgeKind, NodeKind};

    let mut raw_sum: u64 = 0;
    let mut attributed_sum: u64 = 0;
    let mut saw_qualifying_edge = false;

    for (_, _, dst_tid, weight) in cascaded.all_edges() {
        // §7.3 predicate: `e.dst` is an IO pseudo-thread.
        let dst_is_io_pseudo = cascaded.node_index(&dst_tid).is_some_and(|idx| {
            matches!(
                cascaded.node_weight(idx).kind,
                NodeKind::PseudoDisk | NodeKind::PseudoNic | NodeKind::PseudoSoftirq
            )
        });
        if !dst_is_io_pseudo {
            continue;
        }
        // §7.3 predicate: `e.kind != EdgeKind::SyntheticClosureReturn`.
        if weight.kind == EdgeKind::SyntheticClosureReturn {
            continue;
        }
        saw_qualifying_edge = true;
        raw_sum += weight.raw_wait_ms;
        attributed_sum += weight.attributed_delay_ms;
    }

    if !saw_qualifying_edge || raw_sum == 0 {
        return None;
    }

    // Cast via `as` is safe: u64→f64 loses precision past 2^53 but for
    // millisecond wait sums in a single capture this is comfortable.
    #[allow(clippy::cast_precision_loss)]
    Some(attributed_sum as f64 / raw_sum as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::event::{EventType, WperfEvent};
    use crate::format::writer::WperfWriter;
    use crate::graph::types::ThreadId;
    use std::io::Cursor;

    fn write_and_read(events: &[WperfEvent], drop_count: u64) -> WperfReader<Cursor<Vec<u8>>> {
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = WperfWriter::new(&mut cursor).unwrap();
        for ev in events {
            writer.write_event(ev).unwrap();
        }
        writer.finish(drop_count).unwrap();
        let buf = cursor.into_inner();
        WperfReader::open(Cursor::new(buf)).unwrap()
    }

    fn switch_event(ts: u64, prev_tid: u32, next_tid: u32, prev_state: u8) -> WperfEvent {
        WperfEvent {
            timestamp_ns: ts,
            pid: 0,
            tid: 0,
            prev_tid,
            next_tid,
            prev_pid: 0,
            next_pid: 0,
            cpu: 0,
            event_type: EventType::Switch as u8,
            prev_state,
            flags: 0,
        }
    }

    fn wakeup_event(ts: u64, source: u32, target: u32) -> WperfEvent {
        WperfEvent {
            timestamp_ns: ts,
            pid: 0,
            tid: 0,
            prev_tid: source,
            next_tid: target,
            prev_pid: 0,
            next_pid: 0,
            cpu: 0,
            event_type: EventType::Wakeup as u8,
            prev_state: 0,
            flags: 0,
        }
    }

    #[test]
    fn attributed_delay_ratio_none_when_no_io_edges() {
        // Non-IO graph: ratio should be None (not measured).
        use crate::graph::types::{NodeKind, TimeWindow};
        use crate::graph::wfg::WaitForGraph;

        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(100), NodeKind::UserThread);
        g.add_node(ThreadId(200), NodeKind::UserThread);
        g.add_edge(ThreadId(100), ThreadId(200), TimeWindow::new(0, 100));

        assert_eq!(compute_io_attributed_delay_ratio(&g), None);
    }

    #[test]
    fn attributed_delay_ratio_some_on_io_edges() {
        // IoBlock edges contribute to the ratio; other edges are ignored.
        use crate::graph::types::{DISK_TID, NodeKind, TimeWindow, WaitType};
        use crate::graph::wfg::WaitForGraph;

        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(100), NodeKind::UserThread);
        g.add_node(ThreadId(DISK_TID), NodeKind::PseudoDisk);
        // User→Disk (IoBlock), raw=10
        g.add_edge_with_wait_type(
            ThreadId(100),
            ThreadId(DISK_TID),
            TimeWindow::new(0, 10),
            WaitType::IoBlock,
        );
        // Disk→User (IoBlock), raw=10
        g.add_edge_with_wait_type(
            ThreadId(DISK_TID),
            ThreadId(100),
            TimeWindow::new(0, 10),
            WaitType::IoBlock,
        );

        // EdgeWeight::new sets raw_wait_ms = attributed_delay_ms = duration,
        // so the ratio pre-cascade equals 1.0.
        let ratio = compute_io_attributed_delay_ratio(&g).expect("IO edges present");
        assert!(
            (ratio - 1.0).abs() < 1e-9,
            "expected ratio=1.0, got {ratio}"
        );
    }

    #[test]
    fn attributed_delay_ratio_none_when_all_io_edges_zero_duration() {
        // Sub-ms I/Os collapse to zero-duration windows. Ratio is undefined
        // when the raw denominator sums to zero — must return None, not NaN.
        use crate::graph::types::{DISK_TID, NodeKind, TimeWindow, WaitType};
        use crate::graph::wfg::WaitForGraph;

        let mut g = WaitForGraph::new();
        g.add_node(ThreadId(100), NodeKind::UserThread);
        g.add_node(ThreadId(DISK_TID), NodeKind::PseudoDisk);
        g.add_edge_with_wait_type(
            ThreadId(100),
            ThreadId(DISK_TID),
            TimeWindow::new(5, 5),
            WaitType::IoBlock,
        );

        assert_eq!(compute_io_attributed_delay_ratio(&g), None);
    }

    #[test]
    fn empty_trace_produces_valid_report() {
        let mut reader = write_and_read(&[], 0);
        let report =
            build_report(&mut reader, crate::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS).unwrap();

        assert_eq!(report.cascade.edges.len(), 0);
        assert!(report.critical_path.is_none());
        assert!(report.knots.is_empty());
        assert_eq!(report.stats.events_read, 0);
    }

    #[test]
    fn figure4_report() {
        // T1 waits on T2, T2 waits on T3 — linear chain
        let events = vec![
            switch_event(1_000_000, 100, 200, 1), // T100 off-CPU
            wakeup_event(2_000_000, 200, 100),    // T200 wakes T100
            switch_event(3_000_000, 200, 100, 0), // T100 back on-CPU
        ];
        let mut reader = write_and_read(&events, 0);
        let report =
            build_report(&mut reader, crate::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS).unwrap();

        assert_eq!(report.stats.events_read, 3);
        assert_eq!(report.stats.correlation.edges_created, 1);
        assert_eq!(report.cascade.edges.len(), 1);
        assert!(report.cascade.graph_metrics.invariants_ok);

        // Edge: T100 → T200 (T100 waited on T200)
        let edge = &report.cascade.edges[0];
        assert_eq!(edge.src, ThreadId(100));
        assert_eq!(edge.dst, ThreadId(200));
        assert!(edge.raw_wait_ms > 0);
    }

    #[test]
    fn report_includes_drop_count() {
        let mut reader = write_and_read(&[], 42);
        let report =
            build_report(&mut reader, crate::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS).unwrap();

        assert_eq!(report.health.drop_count, Some(42));
    }

    #[test]
    fn health_metrics_actual_values() {
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];
        let mut reader = write_and_read(&events, 7);
        let report =
            build_report(&mut reader, crate::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS).unwrap();

        // Actual metrics — wired from pipeline
        assert!(report.health.invariants_ok);
        assert_eq!(report.health.drop_count, Some(7));
        assert_eq!(report.health.unmatched_wakeup_count, 0);
    }

    #[test]
    fn health_metrics_unmatched_wakeup() {
        // Wakeup with no matching off-CPU switch → unmatched
        let events = vec![wakeup_event(1_000_000, 200, 100)];
        let mut reader = write_and_read(&events, 0);
        let report =
            build_report(&mut reader, crate::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS).unwrap();

        assert_eq!(report.health.unmatched_wakeup_count, 1);
    }

    #[test]
    fn health_metrics_unavailable_are_null() {
        let mut reader = write_and_read(&[], 0);
        let report =
            build_report(&mut reader, crate::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS).unwrap();

        // Unavailable metrics — not yet measured, serialized as null
        assert!(report.health.partial_stack_count.is_none());
        assert!(report.health.cascade_depth_truncation_count.is_none());
        // false_wakeup_filtered_count is now active (§2.5)
        assert_eq!(report.health.false_wakeup_filtered_count, Some(0));
    }

    #[test]
    fn report_json_roundtrip() {
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];
        let mut reader = write_and_read(&events, 5);
        let report =
            build_report(&mut reader, crate::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS).unwrap();

        let json = serde_json::to_value(&report).unwrap();
        assert!(json["cascade"]["edges"].is_array());
        assert!(json["cascade"]["graph_metrics"]["invariants_ok"].is_boolean());
        assert!(json["critical_path"].is_object() || json["critical_path"].is_null());
        assert!(json["stats"]["events_read"].is_number());

        // Actual health metrics in JSON
        assert_eq!(json["health"]["invariants_ok"], true);
        assert_eq!(json["health"]["drop_count"], 5);
        assert_eq!(json["health"]["unmatched_wakeup_count"], 0);

        // Unavailable metrics are null in JSON
        assert!(json["health"]["partial_stack_count"].is_null());
        assert!(json["health"]["cascade_depth_truncation_count"].is_null());
        // false_wakeup_filtered_count is now active (§2.5)
        assert_eq!(json["health"]["false_wakeup_filtered_count"], 0);
    }

    #[test]
    fn corrupted_metadata_propagates_error() {
        // Write a valid trace, then corrupt the section table so the metadata
        // size exceeds MAX_PAYLOAD_SIZE — build_report must return an error,
        // not silently produce drop_count: null.
        let mut cursor = Cursor::new(Vec::new());
        let writer = WperfWriter::new(&mut cursor).unwrap();
        writer.finish(0).unwrap();
        let mut buf = cursor.into_inner();

        // The section table entry is at header.section_table_offset.
        // Entry layout: section_id(4) + offset(8) + size(8) = 20 bytes.
        // Corrupt the size field (bytes 12..20 of the entry) to exceed MAX_PAYLOAD_SIZE.
        #[allow(clippy::cast_possible_truncation)] // test-only, file is tiny
        let st_offset = u64::from_le_bytes(buf[16..24].try_into().unwrap()) as usize;
        let size_field_offset = st_offset + 12; // skip section_id(4) + offset(8)
        let bad_size: u64 = (16 * 1024 * 1024) + 1; // MAX_PAYLOAD_SIZE + 1
        buf[size_field_offset..size_field_offset + 8].copy_from_slice(&bad_size.to_le_bytes());

        let mut reader = WperfReader::open(Cursor::new(buf)).unwrap();
        let result = build_report(&mut reader, crate::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS);
        assert!(result.is_err(), "corrupted metadata must propagate error");
    }

    #[test]
    fn error_display() {
        let err = ReportError::Io(std::io::Error::other("test"));
        assert!(format!("{err}").contains("I/O error"));

        let err = ReportError::Pipeline(PipelineError::Reader(ReaderError::Io(
            std::io::Error::other("test"),
        )));
        assert!(format!("{err}").contains("pipeline error"));

        let err = ReportError::GraphvizNotFound;
        let msg = format!("{err}");
        assert!(msg.contains("not found in PATH"));
        assert!(msg.contains("--format dot"));

        let err = ReportError::GraphvizFailed {
            exit_code: Some(1),
            stderr: "syntax error".to_string(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("exit code 1"));
        assert!(msg.contains("syntax error"));

        let err = ReportError::GraphvizFailed {
            exit_code: None,
            stderr: String::new(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("failed"));
    }

    #[test]
    fn error_source() {
        let err = ReportError::Io(std::io::Error::other("test"));
        assert!(std::error::Error::source(&err).is_some());

        let err = ReportError::GraphvizNotFound;
        assert!(std::error::Error::source(&err).is_none());

        let err = ReportError::GraphvizFailed {
            exit_code: Some(1),
            stderr: "test".into(),
        };
        assert!(std::error::Error::source(&err).is_none());
    }
}
