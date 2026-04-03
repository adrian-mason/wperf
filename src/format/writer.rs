//! .wperf file writer — header + TLV-wrapped events + header backfill.
//!
//! Usage:
//! ```ignore
//! let file = File::create("trace.wperf")?;
//! let mut w = WperfWriter::new(BufWriter::new(file))?;
//! w.write_event(&event)?;
//! w.finish(drop_count)?;   // backfills header
//! ```

use std::io::{self, Seek, SeekFrom, Write};

use super::event::{WperfEvent, EVENT_SIZE};
use super::header::WprfHeader;

/// TLV record type for scheduling events.
const REC_TYPE_SCHED_EVENT: u8 = 1;

/// TLV record header size: 1 byte type + 4 bytes length.
const TLV_HEADER_SIZE: usize = 5;

/// Writer for .wperf binary files.
///
/// Writes a 64B header on creation, then TLV-wrapped events.
/// Call [`finish`] to backfill the header with final offsets and counts.
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

        // TLV header
        self.inner.write_all(&[REC_TYPE_SCHED_EVENT])?;
        self.inner
            .write_all(&(EVENT_SIZE as u32).to_le_bytes())?;

        // Event payload
        event.write_to(&mut self.inner)?;

        self.event_count += 1;

        // Periodically update data_section_end_offset for crash recovery.
        // Every 1024 events, flush the current write position into the header
        // so a crash-recovery reader knows how far valid data extends.
        if self.event_count % 1024 == 0 {
            self.update_data_offset()?;
        }

        Ok(())
    }

    /// Finalize the file: backfill the header with final metadata.
    ///
    /// `drop_count` is the number of events dropped (from BPF ring buffer
    /// overflow or perf buffer lost callbacks).
    pub fn finish(mut self, drop_count: u64) -> io::Result<W> {
        // Record final data section end offset
        let end_pos = self.inner.stream_position()?;

        // Backfill header
        self.header.data_section_end_offset = end_pos;
        // section_table_offset remains 0 — Phase 1 has no footer sections.
        // feature_bitmap[0] bit 0 = has timestamps
        self.header.feature_bitmap[0] |= 0x01;

        // Encode event_count and drop_count into feature_bitmap reserved area.
        // Use bytes 8..16 for event_count, 16..24 for drop_count.
        // These are within the 32-byte feature_bitmap but reserved for
        // format-level metadata rather than capability flags.
        self.header.feature_bitmap[8..16]
            .copy_from_slice(&self.event_count.to_le_bytes());
        self.header.feature_bitmap[16..24]
            .copy_from_slice(&drop_count.to_le_bytes());

        // Seek to beginning and rewrite header
        self.inner.seek(SeekFrom::Start(0))?;
        self.header.write_to(&mut self.inner)?;

        // Seek back to end
        self.inner.seek(SeekFrom::Start(end_pos))?;

        Ok(self.inner)
    }

    /// Number of events written so far.
    pub fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Update the data_section_end_offset in the header for crash recovery.
    fn update_data_offset(&mut self) -> io::Result<()> {
        let current_pos = self.inner.stream_position()?;
        self.header.data_section_end_offset = current_pos;

        // Seek to the data_section_end_offset field (offset 8 in header)
        self.inner.seek(SeekFrom::Start(8))?;
        self.inner
            .write_all(&current_pos.to_le_bytes())?;

        // Seek back to where we were
        self.inner.seek(SeekFrom::Start(current_pos))?;

        Ok(())
    }
}

/// Read event_count from a finalized header's feature_bitmap.
pub fn read_event_count(header: &WprfHeader) -> u64 {
    u64::from_le_bytes(header.feature_bitmap[8..16].try_into().unwrap())
}

/// Read drop_count from a finalized header's feature_bitmap.
pub fn read_drop_count(header: &WprfHeader) -> u64 {
    u64::from_le_bytes(header.feature_bitmap[16..24].try_into().unwrap())
}

/// Read a TLV record header: returns (rec_type, payload_length).
pub fn read_tlv_header<R: io::Read>(r: &mut R) -> io::Result<(u8, u32)> {
    let mut buf = [0u8; TLV_HEADER_SIZE];
    r.read_exact(&mut buf)?;
    let rec_type = buf[0];
    let length = u32::from_le_bytes(buf[1..5].try_into().unwrap());
    Ok((rec_type, length))
}

/// Maximum allowed TLV payload size (16MB, ADR-010 DoS protection).
pub const MAX_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::event::EventType;
    use crate::format::header::HEADER_SIZE;
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
        assert_eq!(read_event_count(&header), 1);
        assert_eq!(read_drop_count(&header), 0);
        assert_eq!(
            header.data_section_end_offset,
            (HEADER_SIZE + TLV_HEADER_SIZE + EVENT_SIZE) as u64
        );

        // Read back TLV record
        let (rec_type, length) = read_tlv_header(&mut buf).unwrap();
        assert_eq!(rec_type, REC_TYPE_SCHED_EVENT);
        assert_eq!(length, EVENT_SIZE as u32);

        let parsed = WperfEvent::read_from(&mut buf).unwrap();
        assert_eq!(parsed, ev);
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

        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();
        assert_eq!(read_event_count(&header), 10);
        assert_eq!(read_drop_count(&header), 5);

        let expected_size =
            HEADER_SIZE + 10 * (TLV_HEADER_SIZE + EVENT_SIZE);
        assert_eq!(header.data_section_end_offset, expected_size as u64);
    }

    #[test]
    fn empty_file_no_events() {
        let buf = Cursor::new(Vec::new());
        let w = WperfWriter::new(buf).unwrap();
        let mut buf = w.finish(0).unwrap();

        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();
        assert_eq!(read_event_count(&header), 0);
        assert_eq!(header.data_section_end_offset, HEADER_SIZE as u64);
    }

    #[test]
    fn crash_recovery_offset_updated_periodically() {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();

        // Write 1024 events to trigger one crash-recovery update
        for i in 0..1024 {
            let ev = make_event(i * 1000, EventType::Switch);
            w.write_event(&ev).unwrap();
        }

        // After 1024 events, data_section_end_offset should be updated
        let expected_offset =
            HEADER_SIZE as u64 + 1024 * (TLV_HEADER_SIZE + EVENT_SIZE) as u64;
        assert_eq!(w.header.data_section_end_offset, expected_offset);
    }

    #[test]
    fn file_total_size() {
        let n = 5u64;
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();
        for i in 0..n {
            w.write_event(&make_event(i, EventType::Wakeup)).unwrap();
        }
        let buf = w.finish(0).unwrap();
        let total = buf.into_inner().len();
        assert_eq!(
            total,
            HEADER_SIZE + (n as usize) * (TLV_HEADER_SIZE + EVENT_SIZE)
        );
    }

    #[test]
    fn drop_count_preserved() {
        let buf = Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).unwrap();
        w.write_event(&make_event(1000, EventType::Switch)).unwrap();
        let mut buf = w.finish(42).unwrap();

        buf.seek(SeekFrom::Start(0)).unwrap();
        let header = WprfHeader::read_from(&mut buf).unwrap();
        assert_eq!(read_drop_count(&header), 42);
    }
}
