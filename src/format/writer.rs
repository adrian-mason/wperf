//! .wperf file writer — header + TLV-wrapped events + footer metadata.
//!
//! Usage:
//! ```ignore
//! let file = File::create("trace.wperf")?;
//! let mut w = WperfWriter::new(BufWriter::new(file))?;
//! w.write_event(&event)?;
//! w.finish(drop_count)?;   // writes footer + backfills header
//! ```

use std::io::{self, Seek, SeekFrom, Write};

use super::event::{EVENT_SIZE, WperfEvent};
use super::header::WprfHeader;

/// TLV record type for scheduling events.
pub const REC_TYPE_SCHED_EVENT: u8 = 1;

/// TLV record header size: 1 byte type + 4 bytes length.
pub const TLV_HEADER_SIZE: usize = 5;

/// Footer section IDs (per final-design.md §4.3).
pub const SECTION_ID_METADATA: u32 = 3;

/// Footer section entry size: `section_id(u32)` + offset(u64) + size(u64) = 20 bytes.
const SECTION_ENTRY_SIZE: usize = 20;

/// Writer for .wperf binary files.
///
/// Writes a 64B header on creation, then TLV-wrapped events.
/// Call [`finish`] to write the footer and backfill the header.
pub struct WperfWriter<W: Write + Seek> {
    inner: W,
    header: WprfHeader,
    event_count: u64,
    start_timestamp_ns: Option<u64>,
    end_timestamp_ns: u64,
}

impl<W: Write + Seek> WperfWriter<W> {
    /// Create a new writer, writing the initial header.
    pub fn new(mut inner: W) -> io::Result<Self> {
        let header = WprfHeader::new();
        header.write_to(&mut inner)?;

        Ok(Self {
            inner,
            header,
            event_count: 0,
            start_timestamp_ns: None,
            end_timestamp_ns: 0,
        })
    }

    /// Write a single event wrapped in a TLV record.
    ///
    /// TLV format: `rec_type(u8) + length(u32 LE) + payload(EVENT_SIZE bytes)`
    pub fn write_event(&mut self, event: &WperfEvent) -> io::Result<()> {
        // Track timestamps
        if self.start_timestamp_ns.is_none() {
            self.start_timestamp_ns = Some(event.timestamp_ns);
        }
        self.end_timestamp_ns = self.end_timestamp_ns.max(event.timestamp_ns);

        // TLV header + event payload in a single write_all (45 bytes).
        let mut record = [0u8; TLV_HEADER_SIZE + EVENT_SIZE];
        record[0] = REC_TYPE_SCHED_EVENT;
        #[allow(clippy::cast_possible_truncation)]
        record[1..5].copy_from_slice(&(EVENT_SIZE as u32).to_le_bytes());
        record[5..].copy_from_slice(&event.to_bytes());
        self.inner.write_all(&record)?;

        self.event_count += 1;

        // Periodically update data_section_end_offset for crash recovery.
        // Every 8192 events (~368 KiB of TLV data), flush the current write
        // position into the header so a crash-recovery reader knows how far
        // valid data extends. Spec §4.4 requires no granularity SLA.
        if self.event_count.is_multiple_of(8192) {
            self.update_data_offset()?;
        }

        Ok(())
    }

    /// Finalize the file: write footer metadata section, then backfill header.
    ///
    /// `drop_count` is the number of events dropped (from BPF ring buffer
    /// overflow or perf buffer lost callbacks).
    ///
    /// File layout after finish:
    /// ```text
    /// [64B Header] [TLV events...] [Metadata payload] [Section Table] [EOF]
    ///                               ^                  ^
    ///                               metadata_offset    section_table_offset
    /// ```
    pub fn finish(mut self, drop_count: u64) -> io::Result<W> {
        // Record data section end offset (after all events, before footer)
        let data_end = self.inner.stream_position()?;
        self.header.data_section_end_offset = data_end;

        // --- Write metadata payload ---
        let metadata_offset = data_end;
        let metadata = build_metadata(self.event_count, drop_count);
        self.inner.write_all(&metadata)?;
        let metadata_size = metadata.len() as u64;

        // --- Write section table ---
        let section_table_offset = self.inner.stream_position()?;
        // Section entry: id(u32) + offset(u64) + size(u64)
        self.inner.write_all(&SECTION_ID_METADATA.to_le_bytes())?;
        self.inner.write_all(&metadata_offset.to_le_bytes())?;
        self.inner.write_all(&metadata_size.to_le_bytes())?;

        // --- Backfill header ---
        self.header.section_table_offset = section_table_offset;
        // feature_bitmap[0] bit 0 = has timestamps
        self.header.feature_bitmap[0] |= 0x01;

        let final_pos = self.inner.stream_position()?;

        // Seek to beginning and rewrite header
        self.inner.seek(SeekFrom::Start(0))?;
        self.header.write_to(&mut self.inner)?;

        // Seek back to end
        self.inner.seek(SeekFrom::Start(final_pos))?;

        Ok(self.inner)
    }

    /// Write a raw event (already in wire format) wrapped in a TLV record.
    ///
    /// Skips the `from_bytes`/`to_bytes` roundtrip — the caller guarantees
    /// `raw` is exactly `EVENT_SIZE` bytes in the correct wire layout.
    pub fn write_event_raw(&mut self, raw: &[u8; EVENT_SIZE]) -> io::Result<()> {
        // Extract timestamp (bytes 0..8, little-endian u64) without full parse.
        let ts = u64::from_le_bytes(raw[0..8].try_into().unwrap());
        if self.start_timestamp_ns.is_none() {
            self.start_timestamp_ns = Some(ts);
        }
        self.end_timestamp_ns = self.end_timestamp_ns.max(ts);

        let mut record = [0u8; TLV_HEADER_SIZE + EVENT_SIZE];
        record[0] = REC_TYPE_SCHED_EVENT;
        #[allow(clippy::cast_possible_truncation)]
        record[1..5].copy_from_slice(&(EVENT_SIZE as u32).to_le_bytes());
        record[5..].copy_from_slice(raw);
        self.inner.write_all(&record)?;

        self.event_count += 1;

        if self.event_count.is_multiple_of(8192) {
            self.update_data_offset()?;
        }

        Ok(())
    }

    /// Number of events written so far.
    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Update the `data_section_end_offset` in the header for crash recovery.
    fn update_data_offset(&mut self) -> io::Result<()> {
        let current_pos = self.inner.stream_position()?;
        self.header.data_section_end_offset = current_pos;

        // Seek to the data_section_end_offset field (offset 8 in header)
        self.inner.seek(SeekFrom::Start(8))?;
        self.inner.write_all(&current_pos.to_le_bytes())?;

        // Seek back to where we were
        self.inner.seek(SeekFrom::Start(current_pos))?;

        Ok(())
    }
}

/// Build a binary metadata payload.
///
/// Format: sequence of `key_len(u16) + key(bytes) + value_len(u16) + value(bytes)`.
/// Phase 1 keys: "`EVENT_COUNT`", "`DROP_COUNT`".
fn build_metadata(event_count: u64, drop_count: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    write_meta_entry(&mut buf, b"EVENT_COUNT", &event_count.to_le_bytes());
    write_meta_entry(&mut buf, b"DROP_COUNT", &drop_count.to_le_bytes());
    buf
}

#[allow(clippy::cast_possible_truncation)] // keys/values are short literals, always fit u16
fn write_meta_entry(buf: &mut Vec<u8>, key: &[u8], value: &[u8]) {
    buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(&(value.len() as u16).to_le_bytes());
    buf.extend_from_slice(value);
}

/// Parse metadata from a footer section payload.
/// Returns (`event_count`, `drop_count`) if found.
pub fn parse_metadata(data: &[u8]) -> (Option<u64>, Option<u64>) {
    let mut event_count = None;
    let mut drop_count = None;
    let mut pos = 0;

    while pos + 4 <= data.len() {
        let key_len = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        if pos + key_len + 2 > data.len() {
            break;
        }
        let key = &data[pos..pos + key_len];
        pos += key_len;

        let val_len = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        if pos + val_len > data.len() {
            break;
        }
        let value = &data[pos..pos + val_len];
        pos += val_len;

        if key == b"EVENT_COUNT" && val_len == 8 {
            event_count = Some(u64::from_le_bytes(value.try_into().unwrap()));
        } else if key == b"DROP_COUNT" && val_len == 8 {
            drop_count = Some(u64::from_le_bytes(value.try_into().unwrap()));
        }
    }

    (event_count, drop_count)
}

/// Read a section table entry: returns (`section_id`, offset, size).
pub fn read_section_entry<R: io::Read>(r: &mut R) -> io::Result<(u32, u64, u64)> {
    let mut buf = [0u8; SECTION_ENTRY_SIZE];
    r.read_exact(&mut buf)?;
    let id = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let offset = u64::from_le_bytes(buf[4..12].try_into().unwrap());
    let size = u64::from_le_bytes(buf[12..20].try_into().unwrap());
    Ok((id, offset, size))
}

/// Read a TLV record header: returns (`rec_type`, `payload_length`).
pub fn read_tlv_header<R: io::Read>(r: &mut R) -> io::Result<(u8, u32)> {
    let mut buf = [0u8; TLV_HEADER_SIZE];
    r.read_exact(&mut buf)?;
    let rec_type = buf[0];
    let length = u32::from_le_bytes(buf[1..5].try_into().unwrap());
    Ok((rec_type, length))
}

/// Maximum allowed TLV payload size (16MB, ADR-010 `DoS` protection).
pub const MAX_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::format::event::EventType;
    use crate::format::header::HEADER_SIZE;
    use std::io::{Cursor, Read};

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

    #[test]
    fn write_and_read_back_single_event() {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();

        let ev = make_event(1_000_000, EventType::Switch);
        w.write_event(&ev).unwrap();
        assert_eq!(w.event_count(), 1);

        let mut buf = w.finish(0).unwrap();

        // Read back header
        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();

        let data_end = (HEADER_SIZE + TLV_HEADER_SIZE + EVENT_SIZE) as u64;
        assert_eq!(header.data_section_end_offset, data_end);
        assert!(header.section_table_offset > 0);

        // Read back TLV record
        buf.seek(SeekFrom::Start(HEADER_SIZE as u64)).unwrap();
        let (rec_type, length) = read_tlv_header(&mut buf).unwrap();
        assert_eq!(rec_type, REC_TYPE_SCHED_EVENT);
        assert_eq!(length, EVENT_SIZE as u32);

        let parsed = WperfEvent::read_from(&mut buf).unwrap();
        assert_eq!(parsed, ev);

        // Read footer metadata
        buf.seek(SeekFrom::Start(header.section_table_offset))
            .unwrap();
        let (sec_id, meta_offset, meta_size) = read_section_entry(&mut buf).unwrap();
        assert_eq!(sec_id, SECTION_ID_METADATA);
        assert_eq!(meta_offset, data_end);

        let mut meta_buf = vec![0u8; meta_size as usize];
        buf.seek(SeekFrom::Start(meta_offset)).unwrap();
        buf.read_exact(&mut meta_buf).unwrap();
        let (ec, dc) = parse_metadata(&meta_buf);
        assert_eq!(ec, Some(1));
        assert_eq!(dc, Some(0));
    }

    #[test]
    fn write_multiple_events() {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();

        for i in 0..10 {
            let ev = make_event(i * 1000, EventType::Switch);
            w.write_event(&ev).unwrap();
        }
        assert_eq!(w.event_count(), 10);

        let mut buf = w.finish(5).unwrap();

        // Read metadata from footer
        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();
        buf.seek(SeekFrom::Start(header.section_table_offset))
            .unwrap();
        let (_, meta_offset, meta_size) = read_section_entry(&mut buf).unwrap();
        let mut meta_buf = vec![0u8; meta_size as usize];
        buf.seek(SeekFrom::Start(meta_offset)).unwrap();
        buf.read_exact(&mut meta_buf).unwrap();
        let (ec, dc) = parse_metadata(&meta_buf);
        assert_eq!(ec, Some(10));
        assert_eq!(dc, Some(5));

        let expected_data_end = HEADER_SIZE + 10 * (TLV_HEADER_SIZE + EVENT_SIZE);
        assert_eq!(header.data_section_end_offset, expected_data_end as u64);
    }

    #[test]
    fn empty_file_no_events() {
        let buf = Cursor::new(Vec::new());
        let w = WperfWriter::new(buf).unwrap();
        let mut buf = w.finish(0).unwrap();

        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();
        assert_eq!(header.data_section_end_offset, HEADER_SIZE as u64);
        assert!(header.section_table_offset > 0);

        // Metadata should still have event_count=0
        buf.seek(SeekFrom::Start(header.section_table_offset))
            .unwrap();
        let (_, meta_offset, meta_size) = read_section_entry(&mut buf).unwrap();
        let mut meta_buf = vec![0u8; meta_size as usize];
        buf.seek(SeekFrom::Start(meta_offset)).unwrap();
        buf.read_exact(&mut meta_buf).unwrap();
        let (ec, dc) = parse_metadata(&meta_buf);
        assert_eq!(ec, Some(0));
        assert_eq!(dc, Some(0));
    }

    #[test]
    fn crash_recovery_offset_updated_periodically() {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();

        // Write 8192 events to trigger one crash-recovery update
        for i in 0..8192 {
            let ev = make_event(i * 1000, EventType::Switch);
            w.write_event(&ev).unwrap();
        }

        // After 8192 events, data_section_end_offset should be updated
        let expected_offset = HEADER_SIZE as u64 + 8192 * (TLV_HEADER_SIZE + EVENT_SIZE) as u64;
        assert_eq!(w.header.data_section_end_offset, expected_offset);
    }

    #[test]
    fn file_layout_order() {
        // Verify: header < data_end <= metadata < section_table < EOF
        let n = 5u64;
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();
        for i in 0..n {
            w.write_event(&make_event(i, EventType::Wakeup)).unwrap();
        }
        let mut buf = w.finish(0).unwrap();

        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();
        let total = buf.get_ref().len() as u64;

        assert!(HEADER_SIZE as u64 <= header.data_section_end_offset);
        assert!(header.data_section_end_offset <= header.section_table_offset);
        assert!(header.section_table_offset < total);
    }

    #[test]
    fn drop_count_preserved() {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();
        w.write_event(&make_event(1000, EventType::Switch)).unwrap();
        let mut buf = w.finish(42).unwrap();

        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();
        buf.seek(SeekFrom::Start(header.section_table_offset))
            .unwrap();
        let (_, meta_offset, meta_size) = read_section_entry(&mut buf).unwrap();
        let mut meta_buf = vec![0u8; meta_size as usize];
        buf.seek(SeekFrom::Start(meta_offset)).unwrap();
        buf.read_exact(&mut meta_buf).unwrap();
        let (_, dc) = parse_metadata(&meta_buf);
        assert_eq!(dc, Some(42));
    }

    #[test]
    fn feature_bitmap_not_abused() {
        // Verify that feature_bitmap only has capability flags, no counters
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();
        w.write_event(&make_event(1000, EventType::Switch)).unwrap();
        let mut buf = w.finish(99).unwrap();

        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();

        // Only bit 0 of byte 0 should be set (has timestamps)
        assert_eq!(header.feature_bitmap[0], 0x01);
        // Bytes 8..24 must be zero (not used for counters anymore)
        assert_eq!(&header.feature_bitmap[8..24], &[0u8; 16]);
    }

    #[test]
    fn metadata_roundtrip() {
        let meta = build_metadata(12345, 67);
        let (ec, dc) = parse_metadata(&meta);
        assert_eq!(ec, Some(12345));
        assert_eq!(dc, Some(67));
    }

    #[test]
    fn parse_metadata_unknown_key_not_misread() {
        // Kill mutation: line 197 && → || in parse_metadata
        // An unknown key with val_len==8 must NOT be parsed as DROP_COUNT.
        let mut buf = Vec::new();
        write_meta_entry(&mut buf, b"EVENT_COUNT", &42u64.to_le_bytes());
        write_meta_entry(&mut buf, b"UNKNOWN_KEY", &99u64.to_le_bytes()); // 8-byte value, unknown key
        let (ec, dc) = parse_metadata(&buf);
        assert_eq!(ec, Some(42));
        assert_eq!(dc, None); // must NOT pick up UNKNOWN_KEY as drop_count
    }

    #[test]
    fn parse_metadata_truncated_before_value() {
        // Kill mutations on line 189: pos + val_len > data.len() boundary
        // Truncate metadata after key but before full value
        let full = build_metadata(100, 200);
        // Truncate to cut the second entry's value short
        let truncated = &full[..full.len() - 3];
        let (ec, _dc) = parse_metadata(truncated);
        // First entry should still parse
        assert_eq!(ec, Some(100));
    }

    #[test]
    fn parse_metadata_truncated_before_key() {
        // Kill mutations on line 181: pos + key_len + 2 > data.len() boundary
        // Create metadata with only the key_len field of a second entry
        let mut buf = Vec::new();
        write_meta_entry(&mut buf, b"EVENT_COUNT", &7u64.to_le_bytes());
        // Append a key_len that claims 20 bytes but don't provide them
        buf.extend_from_slice(&20u16.to_le_bytes());
        let (ec, dc) = parse_metadata(&buf);
        assert_eq!(ec, Some(7));
        assert_eq!(dc, None);
    }

    #[test]
    fn write_event_raw_matches_write_event() {
        let ev = make_event(42_000, EventType::Switch);

        let mut buf1 = Cursor::new(Vec::new());
        let mut w1 = WperfWriter::new(&mut buf1).unwrap();
        w1.write_event(&ev).unwrap();
        let buf1 = w1.finish(0).unwrap().clone().into_inner();

        let mut buf2 = Cursor::new(Vec::new());
        let mut w2 = WperfWriter::new(&mut buf2).unwrap();
        w2.write_event_raw(&ev.to_bytes()).unwrap();
        let buf2 = w2.finish(0).unwrap().clone().into_inner();

        assert_eq!(buf1, buf2);
    }

    #[test]
    fn write_event_raw_tracks_timestamps() {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();

        w.write_event_raw(&make_event(100, EventType::Switch).to_bytes())
            .unwrap();
        w.write_event_raw(&make_event(300, EventType::Wakeup).to_bytes())
            .unwrap();
        w.write_event_raw(&make_event(200, EventType::Switch).to_bytes())
            .unwrap();

        assert_eq!(w.start_timestamp_ns, Some(100));
        assert_eq!(w.end_timestamp_ns, 300);
        assert_eq!(w.event_count(), 3);
    }

    #[test]
    fn write_event_raw_crash_recovery() {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();

        for i in 0..8192 {
            let ev = make_event(i * 1000, EventType::Switch);
            w.write_event_raw(&ev.to_bytes()).unwrap();
        }

        let expected_offset = HEADER_SIZE as u64 + 8192 * (TLV_HEADER_SIZE + EVENT_SIZE) as u64;
        assert_eq!(w.header.data_section_end_offset, expected_offset);
    }
}
