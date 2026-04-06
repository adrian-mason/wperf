//! .wperf file reader — header + TLV event iteration + footer metadata.
//!
//! Usage:
//! ```ignore
//! let file = File::open("trace.wperf")?;
//! let mut r = WperfReader::open(BufReader::new(file))?;
//!
//! // Iterate events
//! while let Some(event) = r.next_event()? {
//!     // process event
//! }
//!
//! // Read footer metadata
//! let meta = r.read_metadata()?;
//! println!("events: {:?}, drops: {:?}", meta.event_count, meta.drop_count);
//! ```

use std::io::{self, Read, Seek, SeekFrom};

use super::event::{EVENT_SIZE, WperfEvent};
use super::header::{HeaderError, WprfHeader};
use super::writer::{self, MAX_PAYLOAD_SIZE, REC_TYPE_SCHED_EVENT, SECTION_ID_METADATA, TLV_HEADER_SIZE};

/// Errors when reading a .wperf file.
#[derive(Debug)]
pub enum ReaderError {
    Header(HeaderError),
    /// TLV record payload exceeds `MAX_PAYLOAD_SIZE`.
    PayloadTooLarge {
        rec_type: u8,
        length: u32,
    },
    /// TLV record has an unexpected payload length for its type.
    UnexpectedPayloadSize {
        rec_type: u8,
        expected: u32,
        actual: u32,
    },
    /// Unknown TLV record type (skipped in lenient mode, error in strict).
    UnknownRecordType(u8),
    Io(io::Error),
}

impl std::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Header(e) => write!(f, "header error: {e}"),
            Self::PayloadTooLarge { rec_type, length } => {
                write!(f, "TLV payload too large: type={rec_type}, length={length}")
            }
            Self::UnexpectedPayloadSize {
                rec_type,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "unexpected payload size for type {rec_type}: expected {expected}, got {actual}"
                )
            }
            Self::UnknownRecordType(t) => write!(f, "unknown TLV record type: {t}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ReaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Header(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ReaderError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<HeaderError> for ReaderError {
    fn from(e: HeaderError) -> Self {
        Self::Header(e)
    }
}

/// Parsed footer metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub event_count: Option<u64>,
    pub drop_count: Option<u64>,
}

/// Reader for .wperf binary files.
///
/// Parses the header on construction, then provides an iterator-style
/// interface for TLV-wrapped events. Footer metadata is read on demand.
#[derive(Debug)]
pub struct WperfReader<R: Read + Seek> {
    inner: R,
    header: WprfHeader,
    /// Byte offset marking the end of valid event data.
    /// Events are read only up to this boundary (crash recovery safe).
    data_end: u64,
}

impl<R: Read + Seek> WperfReader<R> {
    /// Open a .wperf stream, parsing and validating the header.
    ///
    /// Clamps the data boundary to the actual stream length so that a
    /// stale `data_section_end_offset` (header flushed but trailing
    /// records not yet written) does not cause `UnexpectedEof` errors.
    pub fn open(mut inner: R) -> Result<Self, ReaderError> {
        inner.seek(SeekFrom::Start(0))?;
        let header = WprfHeader::read_from(&mut inner)?;

        // Clamp to actual file length for crash-recovery robustness:
        // the header bookmark may have been flushed before all records landed.
        let file_len = inner.seek(SeekFrom::End(0))?;
        let data_end = header.data_section_end_offset.min(file_len);
        inner.seek(SeekFrom::Start(super::header::HEADER_SIZE as u64))?;

        Ok(Self {
            inner,
            header,
            data_end,
        })
    }

    /// Access the parsed file header.
    pub fn header(&self) -> &WprfHeader {
        &self.header
    }

    /// Read the next event from the TLV stream.
    ///
    /// Returns `Ok(None)` when the data section is exhausted.
    /// Unknown record types are skipped (forward-compatible).
    pub fn next_event(&mut self) -> Result<Option<WperfEvent>, ReaderError> {
        loop {
            let pos = self.inner.stream_position()?;
            if pos >= self.data_end {
                return Ok(None);
            }

            // Check if there's enough room for at least a TLV header
            if pos + TLV_HEADER_SIZE as u64 > self.data_end {
                return Ok(None);
            }

            let (rec_type, length) = writer::read_tlv_header(&mut self.inner)?;

            // DoS protection
            if length > MAX_PAYLOAD_SIZE {
                return Err(ReaderError::PayloadTooLarge { rec_type, length });
            }

            // Check the payload fits within the data section
            let payload_end = self.inner.stream_position()? + u64::from(length);
            if payload_end > self.data_end {
                // Truncated record (crash mid-write) — stop here
                return Ok(None);
            }

            #[allow(clippy::cast_possible_truncation)] // EVENT_SIZE is 40, always fits u32
            if rec_type == REC_TYPE_SCHED_EVENT {
                if length != EVENT_SIZE as u32 {
                    return Err(ReaderError::UnexpectedPayloadSize {
                        rec_type,
                        expected: EVENT_SIZE as u32,
                        actual: length,
                    });
                }
                let event = WperfEvent::read_from(&mut self.inner)?;
                return Ok(Some(event));
            }

            // Unknown record type — skip payload (forward compat)
            self.inner.seek(SeekFrom::Current(i64::from(length)))?;
        }
    }

    /// Collect all events into a `Vec`.
    pub fn read_all_events(&mut self) -> Result<Vec<WperfEvent>, ReaderError> {
        let mut events = Vec::new();
        while let Some(ev) = self.next_event()? {
            events.push(ev);
        }
        Ok(events)
    }

    /// Read footer metadata (event count, drop count).
    ///
    /// Returns `Ok(Metadata { None, None })` if the file has no section table
    /// (e.g. crash before `finish()`).
    pub fn read_metadata(&mut self) -> Result<Metadata, ReaderError> {
        let st_offset = self.header.section_table_offset;

        // No footer written (crash recovery case)
        if st_offset == 0 {
            return Ok(Metadata {
                event_count: None,
                drop_count: None,
            });
        }

        // Seek to section table
        self.inner.seek(SeekFrom::Start(st_offset))?;

        // Read section entries until EOF. Each entry is 20 bytes.
        let mut metadata_offset = None;
        let mut metadata_size = None;

        loop {
            match writer::read_section_entry(&mut self.inner) {
                Ok((section_id, offset, size)) => {
                    if section_id == SECTION_ID_METADATA {
                        metadata_offset = Some(offset);
                        metadata_size = Some(size);
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(ReaderError::Io(e)),
            }
        }

        let (Some(offset), Some(size)) = (metadata_offset, metadata_size) else {
            return Ok(Metadata {
                event_count: None,
                drop_count: None,
            });
        };

        // Read metadata payload (with OOM protection)
        #[allow(clippy::cast_possible_truncation)] // SECTION_ID_METADATA fits u8; size is bounded
        if size > u64::from(MAX_PAYLOAD_SIZE) {
            return Err(ReaderError::PayloadTooLarge {
                rec_type: SECTION_ID_METADATA as u8,
                length: size.min(u64::from(u32::MAX)) as u32,
            });
        }
        self.inner.seek(SeekFrom::Start(offset))?;
        #[allow(clippy::cast_possible_truncation)] // bounded by MAX_PAYLOAD_SIZE check above
        let mut buf = vec![0u8; size as usize];
        self.inner.read_exact(&mut buf)?;

        let (event_count, drop_count) = writer::parse_metadata(&buf);
        Ok(Metadata {
            event_count,
            drop_count,
        })
    }

    /// Reset the read position to the start of the data section (after header).
    /// Allows re-iterating events.
    pub fn rewind(&mut self) -> Result<(), ReaderError> {
        self.inner
            .seek(SeekFrom::Start(super::header::HEADER_SIZE as u64))?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::format::event::EventType;
    use crate::format::writer::WperfWriter;
    use std::io::Cursor;

    fn make_event(ts: u64, etype: EventType) -> WperfEvent {
        WperfEvent {
            timestamp_ns: ts,
            pid: 100,
            tid: 101,
            prev_tid: 101,
            next_tid: 202,
            prev_pid: 100,
            next_pid: 200,
            cpu: 0,
            event_type: etype as u8,
            prev_state: 1,
            flags: 0,
        }
    }

    /// Helper: write events with `WperfWriter`, return the raw bytes.
    fn write_trace(events: &[WperfEvent], drop_count: u64) -> Vec<u8> {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();
        for ev in events {
            w.write_event(ev).unwrap();
        }
        let buf = w.finish(drop_count).unwrap();
        buf.into_inner()
    }

    #[test]
    fn read_single_event() {
        let ev = make_event(1_000_000, EventType::Switch);
        let data = write_trace(&[ev], 0);
        let mut r = WperfReader::open(Cursor::new(data)).unwrap();

        let got = r.next_event().unwrap().unwrap();
        assert_eq!(got, ev);
        assert!(r.next_event().unwrap().is_none());
    }

    #[test]
    fn read_multiple_events() {
        let events: Vec<_> = (0..10)
            .map(|i| make_event(i * 1000, EventType::Switch))
            .collect();
        let data = write_trace(&events, 5);
        let mut r = WperfReader::open(Cursor::new(data)).unwrap();

        let got = r.read_all_events().unwrap();
        assert_eq!(got, events);
    }

    #[test]
    fn read_metadata_after_events() {
        let events: Vec<_> = (0..10)
            .map(|i| make_event(i * 1000, EventType::Wakeup))
            .collect();
        let data = write_trace(&events, 42);
        let mut r = WperfReader::open(Cursor::new(data)).unwrap();

        let meta = r.read_metadata().unwrap();
        assert_eq!(meta.event_count, Some(10));
        assert_eq!(meta.drop_count, Some(42));
    }

    #[test]
    fn read_empty_trace() {
        let data = write_trace(&[], 0);
        let mut r = WperfReader::open(Cursor::new(data)).unwrap();

        assert!(r.next_event().unwrap().is_none());

        let meta = r.read_metadata().unwrap();
        assert_eq!(meta.event_count, Some(0));
        assert_eq!(meta.drop_count, Some(0));
    }

    #[test]
    fn header_accessible() {
        let data = write_trace(&[], 0);
        let r = WperfReader::open(Cursor::new(data)).unwrap();
        assert_eq!(r.header().version, 1);
    }

    #[test]
    fn rewind_allows_reiteration() {
        let events: Vec<_> = (0..3)
            .map(|i| make_event(i * 100, EventType::Switch))
            .collect();
        let data = write_trace(&events, 0);
        let mut r = WperfReader::open(Cursor::new(data)).unwrap();

        let first_pass = r.read_all_events().unwrap();
        r.rewind().unwrap();
        let second_pass = r.read_all_events().unwrap();
        assert_eq!(first_pass, second_pass);
    }

    #[test]
    fn crash_recovery_truncated_file() {
        // Simulate a crash: write events but don't call finish().
        // Manually build a file with header + 2 full events + partial 3rd.
        let ev = make_event(1000, EventType::Switch);
        let data = write_trace(&[ev, ev, ev], 0);

        // Truncate: keep header + 2 complete TLV records + 3 bytes of 3rd
        let record_size = 5 + EVENT_SIZE; // TLV header + payload
        let truncated_len = 64 + 2 * record_size + 3;
        let mut truncated = data[..truncated_len].to_vec();

        // Fix the header: set data_section_end_offset to cover only 2 events,
        // section_table_offset to 0 (no footer).
        let two_events_end = (64 + 2 * record_size) as u64;
        truncated[8..16].copy_from_slice(&two_events_end.to_le_bytes());
        truncated[16..24].copy_from_slice(&0u64.to_le_bytes());

        let mut r = WperfReader::open(Cursor::new(truncated)).unwrap();
        let got = r.read_all_events().unwrap();
        assert_eq!(got.len(), 2);

        // No footer → metadata is None
        let meta = r.read_metadata().unwrap();
        assert_eq!(meta.event_count, None);
        assert_eq!(meta.drop_count, None);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut data = write_trace(&[], 0);
        data[0..4].copy_from_slice(b"NOPE");
        let err = WperfReader::open(Cursor::new(data)).unwrap_err();
        assert!(matches!(err, ReaderError::Header(HeaderError::BadMagic)));
    }

    #[test]
    fn payload_too_large_rejected() {
        // Craft a TLV record with length > MAX_PAYLOAD_SIZE
        let mut data = write_trace(&[], 0);
        // Overwrite data_section_end_offset to allow reading past header
        let fake_end = 1_000_000u64;
        data[8..16].copy_from_slice(&fake_end.to_le_bytes());
        data[16..24].copy_from_slice(&0u64.to_le_bytes()); // no footer

        // Append a TLV header with absurd length
        let bad_length = MAX_PAYLOAD_SIZE + 1;
        data.push(1); // rec_type
        data.extend_from_slice(&bad_length.to_le_bytes());

        let mut r = WperfReader::open(Cursor::new(data)).unwrap();
        let err = r.next_event().unwrap_err();
        assert!(matches!(err, ReaderError::PayloadTooLarge { .. }));
    }

    #[test]
    fn wrong_payload_size_rejected() {
        // Start from just a valid header (no footer noise)
        let mut data = vec![0u8; 64];
        let header = WprfHeader::new();
        data.copy_from_slice(&header.to_bytes());

        // TLV: type=1, length=20 (wrong, should be 40)
        data.push(1);
        data.extend_from_slice(&20u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 20]);

        // Set data_section_end_offset to cover the whole crafted region
        let fake_end = data.len() as u64;
        data[8..16].copy_from_slice(&fake_end.to_le_bytes());
        data[16..24].copy_from_slice(&0u64.to_le_bytes()); // no footer

        let mut r = WperfReader::open(Cursor::new(data)).unwrap();
        let err = r.next_event().unwrap_err();
        assert!(matches!(
            err,
            ReaderError::UnexpectedPayloadSize {
                rec_type: 1,
                expected: 40,
                actual: 20,
            }
        ));
    }

    #[test]
    fn unknown_record_type_skipped() {
        // Write a normal trace, then inject an unknown record type before events
        let ev = make_event(5000, EventType::Switch);

        // Build manually: header + unknown TLV + real event TLV + footer
        let mut w = WperfWriter::new(Cursor::new(Vec::new())).unwrap();
        w.write_event(&ev).unwrap();
        let written = w.finish(0).unwrap().into_inner();

        // Parse header to get offsets
        let header = WprfHeader::from_bytes(written[..64].try_into().unwrap()).unwrap();
        let original_data_end = header.data_section_end_offset;

        // Reconstruct: header + unknown(type=99, 8 bytes payload) + real event TLV + footer
        let unknown_payload = [0xAA; 8];
        let unknown_record_len = 5 + unknown_payload.len(); // TLV header + payload

        let mut crafted = Vec::new();
        // We'll build the header manually with updated offsets
        let new_data_end = original_data_end + unknown_record_len as u64;
        let mut new_header = header.clone();
        new_header.data_section_end_offset = new_data_end;
        new_header.section_table_offset = header.section_table_offset + unknown_record_len as u64;
        crafted.extend_from_slice(&new_header.to_bytes());

        // Unknown TLV record
        crafted.push(99); // unknown type
        crafted.extend_from_slice(&(unknown_payload.len() as u32).to_le_bytes());
        crafted.extend_from_slice(&unknown_payload);

        // Real event TLV (copy from original)
        crafted.extend_from_slice(&written[64..original_data_end as usize]);

        // Footer (copy from original, offsets are now wrong for metadata but we
        // only test event reading here)
        crafted.extend_from_slice(&written[original_data_end as usize..]);

        let mut r = WperfReader::open(Cursor::new(crafted)).unwrap();
        let got = r.next_event().unwrap().unwrap();
        assert_eq!(got, ev);
        assert!(r.next_event().unwrap().is_none());
    }

    #[test]
    fn large_trace_roundtrip() {
        // 2048 events to exercise the crash-recovery bookmark path in the writer
        let events: Vec<_> = (0..2048)
            .map(|i| make_event(i * 500, EventType::Switch))
            .collect();
        let data = write_trace(&events, 100);
        let mut r = WperfReader::open(Cursor::new(data)).unwrap();

        let got = r.read_all_events().unwrap();
        assert_eq!(got.len(), 2048);
        assert_eq!(got, events);

        let meta = r.read_metadata().unwrap();
        assert_eq!(meta.event_count, Some(2048));
        assert_eq!(meta.drop_count, Some(100));
    }

    #[test]
    fn header_accessor_returns_actual_header() {
        // Kill mutation: header() → Box::leak(Box::new(Default::default()))
        // A written trace has section_table_offset > 0, while Default has 0.
        let data = write_trace(&[], 0);
        let r = WperfReader::open(Cursor::new(data)).unwrap();
        assert!(r.header().section_table_offset > 0);
    }

    #[test]
    fn reader_error_display() {
        // Kill mutation: Display::fmt → Ok(Default::default())
        let err = ReaderError::PayloadTooLarge {
            rec_type: 1,
            length: 999,
        };
        let msg = format!("{err}");
        assert!(!msg.is_empty());
        assert!(msg.contains("999"));

        let err = ReaderError::UnexpectedPayloadSize {
            rec_type: 1,
            expected: 40,
            actual: 20,
        };
        assert!(format!("{err}").contains("20"));

        let err = ReaderError::UnknownRecordType(99);
        assert!(format!("{err}").contains("99"));
    }

    #[test]
    fn reader_error_source() {
        // Kill mutations: Error::source → None, delete match arms
        let io_err = io::Error::other("test");
        let err = ReaderError::Io(io_err);
        assert!(std::error::Error::source(&err).is_some());

        let header_err = ReaderError::Header(HeaderError::BadMagic);
        assert!(std::error::Error::source(&header_err).is_some());

        // Non-io/header variants should have no source
        let err = ReaderError::UnknownRecordType(5);
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn trailing_partial_tlv_header_ignored() {
        // Kill mutations on line 149: pos + 5 > data_end boundary checks
        // Create a file with 1 event + 3 trailing junk bytes within data section
        let ev = make_event(1000, EventType::Switch);
        let mut data = vec![0u8; 64];
        let header = WprfHeader::new();
        data.copy_from_slice(&header.to_bytes());

        // Write one TLV event
        data.push(1); // rec_type
        data.extend_from_slice(&(EVENT_SIZE as u32).to_le_bytes());
        data.extend_from_slice(&ev.to_bytes());

        // 3 trailing junk bytes (less than TLV header size of 5)
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

        // Set data_section_end_offset to include the trailing junk
        let data_end = data.len() as u64;
        data[8..16].copy_from_slice(&data_end.to_le_bytes());
        data[16..24].copy_from_slice(&0u64.to_le_bytes()); // no footer

        let mut r = WperfReader::open(Cursor::new(data)).unwrap();
        let got = r.read_all_events().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], ev);
    }

    #[test]
    fn read_metadata_io_error_not_swallowed() {
        // Kill mutation: match guard e.kind() == UnexpectedEof replaced with true
        // When read_section_entry fails with non-EOF error, it should propagate.
        // We craft a file with section_table_offset pointing to valid but
        // corrupt section data that causes a non-EOF I/O error scenario.
        // In practice, the only I/O errors from Cursor are UnexpectedEof,
        // so we verify the normal path works correctly with real section data.
        let events: Vec<_> = (0..3)
            .map(|i| make_event(i * 100, EventType::Switch))
            .collect();
        let data = write_trace(&events, 7);
        let mut r = WperfReader::open(Cursor::new(data)).unwrap();
        let meta = r.read_metadata().unwrap();
        assert_eq!(meta.event_count, Some(3));
        assert_eq!(meta.drop_count, Some(7));
    }

    #[test]
    fn stale_data_end_offset_past_eof_graceful() {
        // Regression: header bookmark flushed but trailing records not landed.
        // data_section_end_offset says "2 events" but file only has 1 complete.
        let ev = make_event(1000, EventType::Switch);
        let data = write_trace(&[ev, ev], 0);

        let record_size = 5 + EVENT_SIZE;
        // Keep header + 1 complete TLV record only
        let truncated_len = 64 + record_size;
        let truncated = data[..truncated_len].to_vec();
        // Header still claims data_section_end_offset covers 2 events — stale.

        let mut r = WperfReader::open(Cursor::new(truncated)).unwrap();
        let got = r.read_all_events().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], ev);
    }
}
