//! Gate 0 #9: wPRF v1 Format Roundtrip Prototype
//!
//! Validates: 64B header + TLV event stream + crash recovery
//! via data_section_end_offset.
//!
//! This is throwaway code — discarded after Gate 0.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

// =============================================================================
// wPRF v1 Header (64 bytes)
// =============================================================================

const MAGIC: [u8; 4] = *b"wPRF";
const VERSION: u8 = 1;
const ENDIAN_LE: u8 = 1;
const ARCH_X86_64: u8 = 0;
const HEADER_SIZE: usize = 64;

#[derive(Debug, Clone, PartialEq)]
struct WprfHeader {
    magic: [u8; 4],
    version: u8,
    endianness: u8,
    host_arch: u8,
    _reserved: u8,
    data_section_end_offset: u64,
    section_table_offset: u64,
    feature_bitmap: [u8; 32],
    reserved_padding: [u8; 8],
}

impl WprfHeader {
    fn new() -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            endianness: ENDIAN_LE,
            host_arch: ARCH_X86_64,
            _reserved: 0,
            data_section_end_offset: 0,
            section_table_offset: 0,
            feature_bitmap: [0u8; 32],
            reserved_padding: [0u8; 8],
        }
    }

    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&self.magic)?;
        w.write_all(&[self.version])?;
        w.write_all(&[self.endianness])?;
        w.write_all(&[self.host_arch])?;
        w.write_all(&[self._reserved])?;
        w.write_all(&self.data_section_end_offset.to_le_bytes())?;
        w.write_all(&self.section_table_offset.to_le_bytes())?;
        w.write_all(&self.feature_bitmap)?;
        w.write_all(&self.reserved_padding)?;
        Ok(())
    }

    fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let mut buf = [0u8; HEADER_SIZE];
        r.read_exact(&mut buf)?;

        let magic = [buf[0], buf[1], buf[2], buf[3]];
        if magic != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad magic"));
        }

        Ok(Self {
            magic,
            version: buf[4],
            endianness: buf[5],
            host_arch: buf[6],
            _reserved: buf[7],
            data_section_end_offset: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            section_table_offset: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            feature_bitmap: buf[24..56].try_into().unwrap(),
            reserved_padding: buf[56..64].try_into().unwrap(),
        })
    }
}

// =============================================================================
// TLV Record Header (5 bytes) + BaseEvent (23 bytes)
// =============================================================================

const REC_TYPE_BASE_EVENT: u8 = 1;
const TLV_HEADER_SIZE: usize = 5;
const BASE_EVENT_SIZE: usize = 23;

#[derive(Debug, Clone, PartialEq)]
struct BaseEvent {
    event_id: u8,
    cpu: u16,
    pid: u32,
    tid: u32,
    timestamp_ns: u64,
    flags: u32,
}

impl BaseEvent {
    fn write_tlv(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&[REC_TYPE_BASE_EVENT])?;
        w.write_all(&(BASE_EVENT_SIZE as u32).to_le_bytes())?;
        w.write_all(&[self.event_id])?;
        w.write_all(&self.cpu.to_le_bytes())?;
        w.write_all(&self.pid.to_le_bytes())?;
        w.write_all(&self.tid.to_le_bytes())?;
        w.write_all(&self.timestamp_ns.to_le_bytes())?;
        w.write_all(&self.flags.to_le_bytes())?;
        Ok(())
    }

    fn read_tlv(r: &mut impl Read) -> io::Result<Self> {
        let mut hdr = [0u8; TLV_HEADER_SIZE];
        r.read_exact(&mut hdr)?;
        let rec_type = hdr[0];
        let length = u32::from_le_bytes(hdr[1..5].try_into().unwrap()) as usize;

        if rec_type != REC_TYPE_BASE_EVENT {
            let mut skip = vec![0u8; length];
            r.read_exact(&mut skip)?;
            return Err(io::Error::new(io::ErrorKind::Other, "unknown rec_type"));
        }
        if length != BASE_EVENT_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad event size"));
        }

        let mut buf = [0u8; BASE_EVENT_SIZE];
        r.read_exact(&mut buf)?;

        Ok(Self {
            event_id: buf[0],
            cpu: u16::from_le_bytes(buf[1..3].try_into().unwrap()),
            pid: u32::from_le_bytes(buf[3..7].try_into().unwrap()),
            tid: u32::from_le_bytes(buf[7..11].try_into().unwrap()),
            timestamp_ns: u64::from_le_bytes(buf[11..19].try_into().unwrap()),
            flags: u32::from_le_bytes(buf[19..23].try_into().unwrap()),
        })
    }
}

// =============================================================================
// Footer Section Table
// =============================================================================

struct SectionEntry {
    id: u32,
    offset: u64,
    size: u64,
}

impl SectionEntry {
    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&self.id.to_le_bytes())?;
        w.write_all(&self.offset.to_le_bytes())?;
        w.write_all(&self.size.to_le_bytes())?;
        Ok(())
    }
}

// =============================================================================
// Writer
// =============================================================================

fn write_wperf(path: &Path, events: &[BaseEvent]) -> io::Result<()> {
    let mut f = File::create(path)?;
    let mut header = WprfHeader::new();
    header.write_to(&mut f)?;

    for event in events {
        event.write_tlv(&mut f)?;
    }

    let data_end = f.stream_position()?;
    let section_table_offset = data_end;
    let metadata = b"DROP_COUNT=0";
    let metadata_offset = section_table_offset + 20;
    let section = SectionEntry {
        id: 3,
        offset: metadata_offset,
        size: metadata.len() as u64,
    };
    section.write_to(&mut f)?;
    f.write_all(metadata)?;

    f.seek(SeekFrom::Start(0))?;
    header.data_section_end_offset = data_end;
    header.section_table_offset = section_table_offset;
    header.write_to(&mut f)?;

    Ok(())
}

// =============================================================================
// Reader (with crash recovery)
// =============================================================================

fn read_wperf(path: &Path) -> io::Result<(WprfHeader, Vec<BaseEvent>)> {
    let mut f = File::open(path)?;
    let header = WprfHeader::read_from(&mut f)?;

    let end = if header.data_section_end_offset > 0 {
        header.data_section_end_offset
    } else {
        let pos = f.seek(SeekFrom::End(0))?;
        f.seek(SeekFrom::Start(HEADER_SIZE as u64))?;
        pos
    };

    f.seek(SeekFrom::Start(HEADER_SIZE as u64))?;
    let record_size = (TLV_HEADER_SIZE + BASE_EVENT_SIZE) as u64;
    let mut events = Vec::new();

    while f.stream_position()? + record_size <= end {
        match BaseEvent::read_tlv(&mut f) {
            Ok(event) => events.push(event),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }

    Ok((header, events))
}

fn read_wperf_recovered(path: &Path) -> io::Result<(WprfHeader, Vec<BaseEvent>)> {
    let mut f = File::open(path)?;
    let file_size = f.metadata()?.len();
    let header = WprfHeader::read_from(&mut f)?;

    let end = if header.data_section_end_offset > HEADER_SIZE as u64
        && header.data_section_end_offset <= file_size
    {
        header.data_section_end_offset
    } else {
        file_size
    };

    f.seek(SeekFrom::Start(HEADER_SIZE as u64))?;
    let record_size = (TLV_HEADER_SIZE + BASE_EVENT_SIZE) as u64;
    let mut events = Vec::new();

    while f.stream_position()? + record_size <= end {
        match BaseEvent::read_tlv(&mut f) {
            Ok(event) => events.push(event),
            Err(_) => break,
        }
    }

    Ok((header, events))
}

// =============================================================================
// Tests
// =============================================================================

fn make_test_events(n: usize) -> Vec<BaseEvent> {
    (0..n)
        .map(|i| BaseEvent {
            event_id: 1,
            cpu: (i % 4) as u16,
            pid: 1000,
            tid: 1000 + i as u32,
            timestamp_ns: 1_000_000_000 + (i as u64 * 100_000),
            flags: 0,
        })
        .collect()
}

fn test_header_layout() {
    let header = WprfHeader::new();
    let mut buf = Vec::new();
    header.write_to(&mut buf).unwrap();

    assert_eq!(buf.len(), 64, "Header must be exactly 64 bytes");
    assert_eq!(&buf[0..4], b"wPRF", "Magic must be wPRF");
    assert_eq!(buf[4], 1, "Version must be 1");
    assert_eq!(buf[7], 0, "_reserved must be 0");

    let parsed = WprfHeader::read_from(&mut &buf[..]).unwrap();
    assert_eq!(header, parsed);

    println!("[PASS] Header: 64 bytes, magic=wPRF, version=1, roundtrip OK");
    println!("       Hex: {:02x?}", &buf[..16]);
}

fn test_roundtrip() {
    let path = Path::new("/tmp/test_roundtrip.wperf");
    let events = make_test_events(10);

    write_wperf(path, &events).unwrap();
    let (header, read_events) = read_wperf(path).unwrap();

    assert_eq!(header.magic, MAGIC);
    assert_eq!(header.version, VERSION);
    assert_eq!(read_events.len(), 10);
    for (i, (w, r)) in events.iter().zip(read_events.iter()).enumerate() {
        assert_eq!(w, r, "Event {i} mismatch");
    }

    let file_size = fs::metadata(path).unwrap().len();
    fs::remove_file(path).ok();

    println!("[PASS] Roundtrip: 10 events, all fields match");
    println!(
        "       File: {} bytes (hdr={}, events=10×{}={}, footer=32)",
        file_size, HEADER_SIZE, TLV_HEADER_SIZE + BASE_EVENT_SIZE,
        10 * (TLV_HEADER_SIZE + BASE_EVENT_SIZE)
    );
}

fn test_truncation_recovery() {
    let path = Path::new("/tmp/test_truncated.wperf");
    let events = make_test_events(10);

    write_wperf(path, &events).unwrap();

    let truncate_at = HEADER_SIZE + 6 * (TLV_HEADER_SIZE + BASE_EVENT_SIZE) + 10;
    let data = fs::read(path).unwrap();
    fs::write(path, &data[..truncate_at]).unwrap();

    let (header, recovered) = read_wperf_recovered(path).unwrap();

    assert_eq!(header.magic, MAGIC);
    assert_eq!(recovered.len(), 6, "Must recover 6 events, got {}", recovered.len());
    for (i, (w, r)) in events[..6].iter().zip(recovered.iter()).enumerate() {
        assert_eq!(w, r, "Recovered event {i} mismatch");
    }

    fs::remove_file(path).ok();
    println!("[PASS] Truncation: cut at byte {}, recovered 6/10 events", truncate_at);
}

fn main() {
    println!("============================================================");
    println!("Gate 0 #9: wPRF v1 Roundtrip Prototype");
    println!("============================================================");

    println!("\n--- Test 1: Header Layout ---");
    test_header_layout();

    println!("\n--- Test 2: Full Roundtrip ---");
    test_roundtrip();

    println!("\n--- Test 3: Truncation Recovery ---");
    test_truncation_recovery();

    println!("\n============================================================");
    println!("All tests passed.");
    println!("============================================================");
}
