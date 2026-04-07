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
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Pipeline(e) => write!(f, "pipeline error: {e}"),
            Self::Cascade(e) => write!(f, "cascade invariant error: {e}"),
        }
    }
}

impl std::error::Error for ReportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Pipeline(e) => Some(e),
            Self::Cascade(e) => Some(e),
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
    /// Unavailable: no false-wakeup filter in Phase 1.
    pub false_wakeup_filtered_count: Option<u64>,
}

/// CLI entry point: opens the file, runs the pipeline, writes output to stdout.
pub fn run(args: &ReportArgs) -> Result<(), ReportError> {
    let file = File::open(&args.input)?;
    let mut reader = WperfReader::open(file).map_err(|e| match e {
        ReaderError::Io(io) => ReportError::Io(io),
        other => ReportError::Pipeline(PipelineError::Reader(other)),
    })?;
    let report = build_report(&mut reader)?;

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
    }
    Ok(())
}

/// Pure, testable report builder: runs the full analysis pipeline on a reader.
pub fn build_report<R: Read + Seek>(
    reader: &mut WperfReader<R>,
) -> Result<ReportOutput, ReportError> {
    // Step 1-3: parse + sort + correlate
    let (wfg, stats) = pipeline::build_wait_for_graph(reader)?;

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
            false_wakeup_filtered_count: None,
        },
    })
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
    fn empty_trace_produces_valid_report() {
        let mut reader = write_and_read(&[], 0);
        let report = build_report(&mut reader).unwrap();

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
        let report = build_report(&mut reader).unwrap();

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
        let report = build_report(&mut reader).unwrap();

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
        let report = build_report(&mut reader).unwrap();

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
        let report = build_report(&mut reader).unwrap();

        assert_eq!(report.health.unmatched_wakeup_count, 1);
    }

    #[test]
    fn health_metrics_unavailable_are_null() {
        let mut reader = write_and_read(&[], 0);
        let report = build_report(&mut reader).unwrap();

        // Unavailable metrics — not yet measured, serialized as null
        assert!(report.health.partial_stack_count.is_none());
        assert!(report.health.cascade_depth_truncation_count.is_none());
        assert!(report.health.false_wakeup_filtered_count.is_none());
    }

    #[test]
    fn report_json_roundtrip() {
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];
        let mut reader = write_and_read(&events, 5);
        let report = build_report(&mut reader).unwrap();

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
        assert!(json["health"]["false_wakeup_filtered_count"].is_null());
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
        let result = build_report(&mut reader);
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
    }

    #[test]
    fn error_source() {
        let err = ReportError::Io(std::io::Error::other("test"));
        assert!(std::error::Error::source(&err).is_some());
    }
}
