//! Snapshot tests for `.wperf` parser output using `insta`.
//!
//! Covers reader round-trip parsing: events, metadata, headers, and error
//! display strings. JSON/report snapshots are deferred to W3 #21.

use std::io::Cursor;

use wperf::format::event::{EventType, WperfEvent};
use wperf::format::header::HEADER_SIZE;
use wperf::format::reader::{ReaderError, WperfReader};
use wperf::format::writer::WperfWriter;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write events into an in-memory `.wperf` file, return raw bytes.
fn write_trace(events: &[WperfEvent], drop_count: u64) -> Vec<u8> {
    let buf = Cursor::new(Vec::new());
    let mut w = WperfWriter::new(buf).unwrap();
    for ev in events {
        w.write_event(ev).unwrap();
    }
    let buf = w.finish(drop_count).unwrap();
    buf.into_inner()
}

/// Write events, then open a reader on the result.
fn write_and_open(events: &[WperfEvent], drop_count: u64) -> WperfReader<Cursor<Vec<u8>>> {
    let data = write_trace(events, drop_count);
    WperfReader::open(Cursor::new(data)).unwrap()
}

fn switch_event(ts: u64, prev_tid: u32, next_tid: u32) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts,
        pid: 100,
        tid: 101,
        prev_tid,
        next_tid,
        prev_pid: 100,
        next_pid: 200,
        cpu: 0,
        event_type: EventType::Switch as u8,
        prev_state: 1,
        flags: 0,
    }
}

fn wakeup_event(ts: u64, source: u32, target: u32) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts,
        pid: 200,
        tid: 201,
        prev_tid: source,
        next_tid: target,
        prev_pid: 200,
        next_pid: 100,
        cpu: 2,
        event_type: EventType::Wakeup as u8,
        prev_state: 0,
        flags: 0,
    }
}

fn exit_event(ts: u64, tid: u32) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts,
        pid: 100,
        tid,
        prev_tid: tid,
        next_tid: 0,
        prev_pid: 100,
        next_pid: 0,
        cpu: 0,
        event_type: EventType::Exit as u8,
        prev_state: 0,
        flags: 0,
    }
}

// ---------------------------------------------------------------------------
// Event parse round-trip snapshots
// ---------------------------------------------------------------------------

#[test]
fn snapshot_single_switch_event() {
    let events = vec![switch_event(1_000_000, 101, 202)];
    let mut reader = write_and_open(&events, 0);
    let parsed = reader.read_all_events().unwrap();
    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn snapshot_single_wakeup_event() {
    let events = vec![wakeup_event(2_000_000, 201, 101)];
    let mut reader = write_and_open(&events, 0);
    let parsed = reader.read_all_events().unwrap();
    insta::assert_debug_snapshot!(parsed);
}

#[test]
fn snapshot_mixed_event_trace() {
    let events = vec![
        switch_event(1_000_000, 101, 202),
        wakeup_event(2_000_000, 201, 101),
        switch_event(3_000_000, 202, 101),
        exit_event(4_000_000, 202),
    ];
    let mut reader = write_and_open(&events, 0);
    let parsed = reader.read_all_events().unwrap();
    insta::assert_debug_snapshot!(parsed);
}

// ---------------------------------------------------------------------------
// Metadata round-trip snapshots
// ---------------------------------------------------------------------------

#[test]
fn snapshot_metadata_with_counts() {
    let events = vec![
        switch_event(1_000_000, 101, 202),
        wakeup_event(2_000_000, 201, 101),
        switch_event(3_000_000, 202, 101),
    ];
    let mut reader = write_and_open(&events, 42);
    // Consume events first so reader position is past data section
    let _ = reader.read_all_events().unwrap();
    let metadata = reader.read_metadata().unwrap();
    insta::assert_debug_snapshot!(metadata);
}

// ---------------------------------------------------------------------------
// Empty trace snapshots
// ---------------------------------------------------------------------------

#[test]
fn snapshot_empty_finished_trace() {
    // Normal empty trace: writer.finish(0) produces a valid file with footer
    let mut reader = write_and_open(&[], 0);
    let events = reader.read_all_events().unwrap();
    let metadata = reader.read_metadata().unwrap();
    insta::assert_debug_snapshot!("empty_finished_events", events);
    insta::assert_debug_snapshot!("empty_finished_metadata", metadata);
}

#[test]
fn snapshot_crash_recovery_no_footer() {
    // Crash recovery: section_table_offset == 0, no footer written.
    // Simulate by writing a valid header + some event data, but with
    // section_table_offset left at 0 (as if the writer crashed before finish).
    let events = vec![switch_event(1_000_000, 101, 202)];
    let data = write_trace(&events, 0);

    // Corrupt: zero out section_table_offset (bytes 16..24 in header)
    let mut corrupted = data;
    corrupted[16..24].copy_from_slice(&0u64.to_le_bytes());

    let mut reader = WperfReader::open(Cursor::new(corrupted)).unwrap();
    let parsed_events = reader.read_all_events().unwrap();
    let metadata = reader.read_metadata().unwrap();
    insta::assert_debug_snapshot!("crash_recovery_events", parsed_events);
    insta::assert_debug_snapshot!("crash_recovery_metadata", metadata);
}

// ---------------------------------------------------------------------------
// Reader error Display snapshots
// ---------------------------------------------------------------------------

#[test]
fn snapshot_error_bad_magic() {
    let mut data = vec![0u8; HEADER_SIZE];
    data[0..4].copy_from_slice(b"NOPE");
    let err = WperfReader::open(Cursor::new(data)).unwrap_err();
    insta::assert_snapshot!("error_bad_magic", format!("{err}"));
}

#[test]
fn snapshot_error_unsupported_version() {
    // Write a valid header, then corrupt the version byte
    let data = write_trace(&[], 0);
    let mut corrupted = data;
    corrupted[4] = 99; // version byte at offset 4
    let err = WperfReader::open(Cursor::new(corrupted)).unwrap_err();
    insta::assert_snapshot!("error_unsupported_version", format!("{err}"));
}

#[test]
fn snapshot_error_payload_too_large() {
    let err = ReaderError::PayloadTooLarge {
        rec_type: 1,
        length: 20_000_000,
    };
    insta::assert_snapshot!("error_payload_too_large", format!("{err}"));
}

#[test]
fn snapshot_error_unexpected_payload_size() {
    let err = ReaderError::UnexpectedPayloadSize {
        rec_type: 1,
        expected: 40,
        actual: 30,
    };
    insta::assert_snapshot!("error_unexpected_payload_size", format!("{err}"));
}

#[test]
fn snapshot_error_unknown_record_type() {
    let err = ReaderError::UnknownRecordType(255);
    insta::assert_snapshot!("error_unknown_record_type", format!("{err}"));
}
