//! Parse + sort + correlate glue — chains `.wperf` file reading through
//! event correlation to produce a [`WaitForGraph`].
//!
//! This module implements the offline pipeline path from `final-design.md
//! §3.2`: parse events via [`WperfReader`], sort using [`WperfEvent`]'s
//! full [`Ord`] for deterministic ordering, then correlate via
//! [`correlate_events`].
//!
//! # Sorting
//!
//! Events are sorted with `Vec::sort_unstable()` over `WperfEvent`'s
//! derived `Ord`, which provides deterministic full-order sorting.
//! `timestamp_ns` is the primary key; remaining fields give a deterministic
//! tie-break for equal-timestamp events. Stability is unnecessary: events
//! with identical `Ord` keys are semantically identical, so reordering them
//! has no observable effect. This matches [`ReorderBuf`]'s live-path
//! `BinaryHeap<Reverse<WperfEvent>>` semantics.
//!
//! # Empty traces
//!
//! An empty trace (zero events) is valid — the pipeline returns an empty
//! graph with zero stats. Upper layers (CLI / report) decide whether to
//! warn the user.

use std::io::{Read, Seek};

use crate::correlate::{self, CorrelationStats};
use crate::format::reader::{ReaderError, WperfReader};
use crate::graph::wfg::WaitForGraph;

/// Pipeline-level error.
#[derive(Debug)]
pub enum PipelineError {
    /// Error reading or parsing the `.wperf` file.
    Reader(ReaderError),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reader(e) => write!(f, "reader error: {e}"),
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Reader(e) => Some(e),
        }
    }
}

impl From<ReaderError> for PipelineError {
    fn from(e: ReaderError) -> Self {
        Self::Reader(e)
    }
}

/// Aggregate statistics from the pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PipelineStats {
    /// Total events read from the `.wperf` file.
    pub events_read: u64,
    /// Correlation statistics from the correlate sub-step.
    pub correlation: CorrelationStats,
}

/// Read, sort, and correlate events from a `.wperf` file into a
/// [`WaitForGraph`].
///
/// Pipeline steps:
/// 1. **Parse**: read all events via [`WperfReader::read_all_events`]
/// 2. **Sort**: full-order sort over `WperfEvent::Ord` for deterministic
///    ordering (see module docs)
/// 3. **Correlate**: [`correlate_events`] produces edges and stats
///
/// Empty traces return `Ok` with an empty graph and zero stats.
pub fn build_wait_for_graph<R: Read + Seek>(
    reader: &mut WperfReader<R>,
) -> Result<(WaitForGraph, PipelineStats), PipelineError> {
    // Step 1: Parse
    let mut events = reader.read_all_events()?;
    let events_read = events.len() as u64;

    // Step 2: Sort — full-order sort over WperfEvent::Ord for deterministic
    // ordering. sort_unstable() is sufficient: events with identical Ord keys
    // are semantically identical. Do NOT use sort_by_key(timestamp_ns) alone,
    // as that loses determinism for equal-timestamp events.
    events.sort_unstable();

    // Step 3: Correlate
    let (graph, correlation) = correlate::correlate_events(&events);

    Ok((
        graph,
        PipelineStats {
            events_read,
            correlation,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::event::{EventType, WperfEvent};
    use crate::format::writer::WperfWriter;
    use crate::graph::types::ThreadId;
    use std::io::Cursor;

    /// Helper: write events to an in-memory .wperf file, then open a reader.
    fn write_and_read(events: &[WperfEvent]) -> WperfReader<Cursor<Vec<u8>>> {
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = WperfWriter::new(&mut cursor).unwrap();
        for ev in events {
            writer.write_event(ev).unwrap();
        }
        writer.finish(0).unwrap();

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
    fn empty_trace_returns_empty_graph() {
        let mut reader = write_and_read(&[]);
        let (graph, stats) = build_wait_for_graph(&mut reader).unwrap();

        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
        assert_eq!(stats.events_read, 0);
        assert_eq!(stats.correlation.events_processed, 0);
    }

    #[test]
    fn single_event_no_edges() {
        let events = vec![switch_event(1_000_000, 100, 200, 0)];
        let mut reader = write_and_read(&events);
        let (graph, stats) = build_wait_for_graph(&mut reader).unwrap();

        assert_eq!(graph.edge_count(), 0);
        assert_eq!(stats.events_read, 1);
        assert_eq!(stats.correlation.events_processed, 1);
    }

    #[test]
    fn simple_chain_produces_edge() {
        let events = vec![
            switch_event(1_000_000, 100, 200, 1),
            wakeup_event(2_000_000, 200, 100),
            switch_event(3_000_000, 200, 100, 0),
        ];
        let mut reader = write_and_read(&events);
        let (graph, stats) = build_wait_for_graph(&mut reader).unwrap();

        assert_eq!(stats.events_read, 3);
        assert_eq!(stats.correlation.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);

        let edges = graph.all_edges();
        assert_eq!(edges[0].1, ThreadId(100));
        assert_eq!(edges[0].2, ThreadId(200));
    }

    #[test]
    fn unsorted_events_are_sorted_before_correlation() {
        // Write events out of timestamp order — pipeline must sort them.
        let events = vec![
            switch_event(3_000_000, 200, 100, 0), // switch-in (written first, ts=3)
            wakeup_event(2_000_000, 200, 100),    // wakeup (written second, ts=2)
            switch_event(1_000_000, 100, 200, 1), // switch-out (written last, ts=1)
        ];
        let mut reader = write_and_read(&events);
        let (graph, stats) = build_wait_for_graph(&mut reader).unwrap();

        // After sorting: switch-out(1) → wakeup(2) → switch-in(3) → 1 edge
        assert_eq!(stats.events_read, 3);
        assert_eq!(stats.correlation.edges_created, 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn pipeline_stats_includes_correlation_stats() {
        let events = vec![
            wakeup_event(1_000_000, 200, 100), // unmatched wakeup
        ];
        let mut reader = write_and_read(&events);
        let (_, stats) = build_wait_for_graph(&mut reader).unwrap();

        assert_eq!(stats.events_read, 1);
        assert_eq!(stats.correlation.unmatched_wakeup_count, 1);
        assert_eq!(stats.correlation.edges_created, 0);
    }

    #[test]
    fn error_display() {
        let err = PipelineError::Reader(ReaderError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        )));
        let msg = format!("{err}");
        assert!(msg.contains("reader error"));
    }

    #[test]
    fn error_source() {
        let err = PipelineError::Reader(ReaderError::Io(std::io::Error::other("test")));
        assert!(std::error::Error::source(&err).is_some());
    }
}
