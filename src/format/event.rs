//! Event types and the 40-byte naturally-aligned event structure.
//!
//! This is the Rust-side mirror of the BPF `struct wperf_event`.
//! Layout must match exactly for zero-copy deserialization.
//!
//! ```text
//!   timestamp_ns:  u64   offset  0   (8B)
//!   pid:           u32   offset  8   (4B)  tgid
//!   tid:           u32   offset 12   (4B)  kernel tid
//!   prev_tid:      u32   offset 16   (4B)
//!   next_tid:      u32   offset 20   (4B)
//!   prev_pid:      u32   offset 24   (4B)
//!   next_pid:      u32   offset 28   (4B)
//!   cpu:           u16   offset 32   (2B)
//!   event_type:    u8    offset 34   (1B)
//!   prev_state:    u8    offset 35   (1B)
//!   flags:         u32   offset 36   (4B)  reserved (0 in Phase 1)
//!   total:                           40B
//! ```

use std::io::{self, Read, Write};

/// Event size in bytes. Naturally aligned, no padding waste.
pub const EVENT_SIZE: usize = 40;

/// Futex operation constants (matching BPF-side definitions from linux/futex.h).
pub mod futex_op {
    pub const FUTEX_WAIT: u32 = 0;
    pub const FUTEX_LOCK_PI: u32 = 6;
    pub const FUTEX_WAIT_BITSET: u32 = 9;
    pub const FUTEX_WAIT_REQUEUE_PI: u32 = 11;
}

/// Event type discriminants — must match BPF `enum wperf_event_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventType {
    Switch = 1,
    Wakeup = 2,
    WakeupNew = 3,
    Exit = 4,
    FutexWait = 5,
}

impl EventType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Switch),
            2 => Some(Self::Wakeup),
            3 => Some(Self::WakeupNew),
            4 => Some(Self::Exit),
            5 => Some(Self::FutexWait),
            _ => None,
        }
    }
}

/// 40-byte event matching the BPF-side `struct wperf_event`.
///
/// `#[repr(C)]` ensures C-compatible layout. Fields are ordered
/// for natural alignment (u64 first, then u32s, then u16, then u8s, then u32).
/// The `flags` field occupies what would otherwise be tail padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub struct WperfEvent {
    pub timestamp_ns: u64,
    pub pid: u32,
    pub tid: u32,
    pub prev_tid: u32,
    pub next_tid: u32,
    pub prev_pid: u32,
    pub next_pid: u32,
    pub cpu: u16,
    pub event_type: u8,
    pub prev_state: u8,
    /// Reserved flags (0 in Phase 1). Phase 2+ may use for:
    /// bit 0: voluntary vs preempted, bit 1: cross-cgroup wakeup,
    /// bits 2-3: event source tier (`tp_btf/raw_tp/kprobe`).
    pub flags: u32,
}

impl WperfEvent {
    /// Serialize to exactly 40 bytes (little-endian).
    pub fn to_bytes(&self) -> [u8; EVENT_SIZE] {
        let mut buf = [0u8; EVENT_SIZE];
        buf[0..8].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf[8..12].copy_from_slice(&self.pid.to_le_bytes());
        buf[12..16].copy_from_slice(&self.tid.to_le_bytes());
        buf[16..20].copy_from_slice(&self.prev_tid.to_le_bytes());
        buf[20..24].copy_from_slice(&self.next_tid.to_le_bytes());
        buf[24..28].copy_from_slice(&self.prev_pid.to_le_bytes());
        buf[28..32].copy_from_slice(&self.next_pid.to_le_bytes());
        buf[32..34].copy_from_slice(&self.cpu.to_le_bytes());
        buf[34] = self.event_type;
        buf[35] = self.prev_state;
        buf[36..40].copy_from_slice(&self.flags.to_le_bytes());
        buf
    }

    /// Parse from exactly 40 bytes (little-endian).
    pub fn from_bytes(buf: &[u8; EVENT_SIZE]) -> Self {
        Self {
            timestamp_ns: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            pid: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            tid: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            prev_tid: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            next_tid: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
            prev_pid: u32::from_le_bytes(buf[24..28].try_into().unwrap()),
            next_pid: u32::from_le_bytes(buf[28..32].try_into().unwrap()),
            cpu: u16::from_le_bytes(buf[32..34].try_into().unwrap()),
            event_type: buf[34],
            prev_state: buf[35],
            flags: u32::from_le_bytes(buf[36..40].try_into().unwrap()),
        }
    }

    /// Write this event to a writer (40 bytes, no TLV wrapper).
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.to_bytes())
    }

    /// Read one event from a reader (40 bytes).
    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; EVENT_SIZE];
        r.read_exact(&mut buf)?;
        Ok(Self::from_bytes(&buf))
    }

    /// Typed event accessor.
    pub fn event_type_enum(&self) -> Option<EventType> {
        EventType::from_u8(self.event_type)
    }

    /// Futex user address (64-bit). Only meaningful for `FutexWait` events.
    /// Stored as: `prev_tid` = lower 32 bits, `next_tid` = upper 32 bits.
    pub fn futex_uaddr(&self) -> u64 {
        u64::from(self.next_tid) << 32 | u64::from(self.prev_tid)
    }

    /// Futex operation (after `FUTEX_CMD_MASK`). Only meaningful for `FutexWait` events.
    /// Stored in `flags` field.
    pub fn futex_op(&self) -> u32 {
        self.flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_switch_event() -> WperfEvent {
        WperfEvent {
            timestamp_ns: 1_000_000_000,
            pid: 100,
            tid: 101,
            prev_tid: 101,
            next_tid: 202,
            prev_pid: 100,
            next_pid: 200,
            cpu: 3,
            event_type: EventType::Switch as u8,
            prev_state: 1, // TASK_INTERRUPTIBLE
            flags: 0,
        }
    }

    fn sample_wakeup_event() -> WperfEvent {
        WperfEvent {
            timestamp_ns: 1_000_050_000,
            pid: 200,
            tid: 201,
            prev_tid: 201, // waker tid
            next_tid: 101, // wakee tid
            prev_pid: 200, // waker tgid
            next_pid: 100, // wakee tgid
            cpu: 5,
            event_type: EventType::Wakeup as u8,
            prev_state: 0,
            flags: 0,
        }
    }

    #[test]
    fn event_size_is_40() {
        assert_eq!(EVENT_SIZE, 40);
        assert_eq!(std::mem::size_of::<WperfEvent>(), EVENT_SIZE);
    }

    #[test]
    fn event_roundtrip() {
        let ev = sample_switch_event();
        let bytes = ev.to_bytes();
        assert_eq!(bytes.len(), EVENT_SIZE);
        let parsed = WperfEvent::from_bytes(&bytes);
        assert_eq!(ev, parsed);
    }

    #[test]
    fn event_io_roundtrip() {
        let ev = sample_wakeup_event();
        let mut buf = Vec::new();
        ev.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), EVENT_SIZE);
        let parsed = WperfEvent::read_from(&mut buf.as_slice()).unwrap();
        assert_eq!(ev, parsed);
    }

    fn sample_futex_event() -> WperfEvent {
        WperfEvent {
            timestamp_ns: 1_000_100_000,
            pid: 100,
            tid: 101,
            prev_tid: 0xDEAD_BEEFu32, // uaddr lower 32
            next_tid: 0x0000_7FFEu32, // uaddr upper 32
            prev_pid: 0,
            next_pid: 0,
            cpu: 2,
            event_type: EventType::FutexWait as u8,
            prev_state: 0,
            flags: futex_op::FUTEX_WAIT,
        }
    }

    #[test]
    fn event_type_enum_known() {
        assert_eq!(EventType::from_u8(1), Some(EventType::Switch));
        assert_eq!(EventType::from_u8(2), Some(EventType::Wakeup));
        assert_eq!(EventType::from_u8(3), Some(EventType::WakeupNew));
        assert_eq!(EventType::from_u8(4), Some(EventType::Exit));
        assert_eq!(EventType::from_u8(5), Some(EventType::FutexWait));
    }

    #[test]
    fn event_type_enum_unknown() {
        assert_eq!(EventType::from_u8(0), None);
        assert_eq!(EventType::from_u8(255), None);
    }

    #[test]
    fn futex_event_roundtrip() {
        let ev = sample_futex_event();
        let bytes = ev.to_bytes();
        let parsed = WperfEvent::from_bytes(&bytes);
        assert_eq!(ev, parsed);
        assert_eq!(parsed.event_type_enum(), Some(EventType::FutexWait));
    }

    #[test]
    fn futex_uaddr_accessor() {
        let ev = sample_futex_event();
        assert_eq!(ev.futex_uaddr(), 0x0000_7FFE_DEAD_BEEFu64);
    }

    #[test]
    fn futex_op_accessor() {
        let ev = sample_futex_event();
        assert_eq!(ev.futex_op(), futex_op::FUTEX_WAIT);

        let mut ev2 = ev;
        ev2.flags = futex_op::FUTEX_LOCK_PI;
        assert_eq!(ev2.futex_op(), futex_op::FUTEX_LOCK_PI);
    }

    #[test]
    fn event_ordering_by_timestamp() {
        let a = sample_switch_event();
        let b = sample_wakeup_event();
        assert!(a < b); // a.timestamp_ns < b.timestamp_ns
    }

    #[test]
    fn event_type_enum_accessor() {
        // Kill mutation: event_type_enum → None
        let ev = sample_switch_event();
        assert_eq!(ev.event_type_enum(), Some(EventType::Switch));

        let ev2 = sample_wakeup_event();
        assert_eq!(ev2.event_type_enum(), Some(EventType::Wakeup));
    }

    #[test]
    fn repr_c_layout() {
        assert_eq!(std::mem::align_of::<WperfEvent>(), 8);
        assert_eq!(std::mem::size_of::<WperfEvent>(), 40);
    }

    #[test]
    fn field_offsets_match_bpf_struct() {
        assert_eq!(std::mem::offset_of!(WperfEvent, timestamp_ns), 0);
        assert_eq!(std::mem::offset_of!(WperfEvent, pid), 8);
        assert_eq!(std::mem::offset_of!(WperfEvent, tid), 12);
        assert_eq!(std::mem::offset_of!(WperfEvent, prev_tid), 16);
        assert_eq!(std::mem::offset_of!(WperfEvent, next_tid), 20);
        assert_eq!(std::mem::offset_of!(WperfEvent, prev_pid), 24);
        assert_eq!(std::mem::offset_of!(WperfEvent, next_pid), 28);
        assert_eq!(std::mem::offset_of!(WperfEvent, cpu), 32);
        assert_eq!(std::mem::offset_of!(WperfEvent, event_type), 34);
        assert_eq!(std::mem::offset_of!(WperfEvent, prev_state), 35);
        assert_eq!(std::mem::offset_of!(WperfEvent, flags), 36);
    }
}
