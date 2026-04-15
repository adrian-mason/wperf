//! Core types for the Wait-For Graph.
//!
//! All time values use `u64` milliseconds for exact integer arithmetic —
//! no floating-point conservation drift.

use serde::Serialize;
use std::fmt;

/// Thread identifier. Pure kernel tid (NOT packed tgid<<32|tid).
/// Negative values represent pseudo-threads (see constants below).
/// tgid is a Phase 1+ UI concern, not an algorithm input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct ThreadId(pub i64);

/// Pseudo-thread IDs for non-thread entities in the Wait-For Graph.
pub const NIC_TID: i64 = -4;
pub const DISK_TID: i64 = -5;
pub const HARDIRQ_TID: i64 = -15;
pub const SOFTIRQ_TID: i64 = -16;

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            NIC_TID => write!(f, "NIC"),
            DISK_TID => write!(f, "Disk"),
            HARDIRQ_TID => write!(f, "HardIRQ"),
            SOFTIRQ_TID => write!(f, "SoftIRQ"),
            id => write!(f, "T{id}"),
        }
    }
}

/// Half-open time interval `[start_ms, end_ms)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimeWindow {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl TimeWindow {
    pub fn new(start_ms: u64, end_ms: u64) -> Self {
        debug_assert!(start_ms <= end_ms, "invalid window: {start_ms} > {end_ms}");
        Self { start_ms, end_ms }
    }

    pub fn duration(&self) -> u64 {
        self.end_ms - self.start_ms
    }

    /// Returns the overlap with `other`, or `None` if disjoint.
    pub fn overlap(&self, other: &TimeWindow) -> Option<TimeWindow> {
        let s = self.start_ms.max(other.start_ms);
        let e = self.end_ms.min(other.end_ms);
        if s < e {
            Some(TimeWindow {
                start_ms: s,
                end_ms: e,
            })
        } else {
            None
        }
    }

    /// True if this window fully contains `point_ms`.
    pub fn contains(&self, point_ms: u64) -> bool {
        point_ms >= self.start_ms && point_ms < self.end_ms
    }
}

/// Wait cause annotation on graph edges.
///
/// `None` = futex tracing was not enabled (not traced).
/// `Some(WaitType::Unknown)` = traced but no matching futex event found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitType {
    Unknown,
    FutexWait,
    FutexLockPi,
    FutexWaitBitset,
    FutexWaitRequeuePi,
}

/// Edge metadata in the Wait-For Graph.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeWeight {
    pub time_window: TimeWindow,
    pub raw_wait_ms: u64,
    pub attributed_delay_ms: u64,
    pub wait_type: Option<WaitType>,
}

impl EdgeWeight {
    pub fn new(time_window: TimeWindow) -> Self {
        let raw = time_window.duration();
        Self {
            time_window,
            raw_wait_ms: raw,
            attributed_delay_ms: raw,
            wait_type: None,
        }
    }

    pub fn with_wait_type(time_window: TimeWindow, wait_type: WaitType) -> Self {
        let mut this = Self::new(time_window);
        this.wait_type = Some(wait_type);
        this
    }
}

/// Node metadata.
#[derive(Debug, Clone, Serialize)]
pub struct NodeWeight {
    pub tid: ThreadId,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum NodeKind {
    UserThread,
    KernelThread,
    PseudoDisk,
    PseudoNic,
    PseudoSoftirq,
}

/// An elementary interval produced by `sweep_line_partition`.
/// Represents a maximal time slice where the set of concurrent
/// wait targets is constant.
#[derive(Debug, Clone)]
pub struct ElementaryInterval {
    pub window: TimeWindow,
    pub targets: Vec<ThreadId>, // sorted for determinism
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_window_duration() {
        assert_eq!(TimeWindow::new(10, 50).duration(), 40);
        assert_eq!(TimeWindow::new(0, 0).duration(), 0);
    }

    #[test]
    fn time_window_overlap() {
        let a = TimeWindow::new(0, 100);
        let b = TimeWindow::new(20, 80);
        assert_eq!(a.overlap(&b), Some(TimeWindow::new(20, 80)));

        let c = TimeWindow::new(50, 150);
        assert_eq!(a.overlap(&c), Some(TimeWindow::new(50, 100)));

        let d = TimeWindow::new(100, 200);
        assert_eq!(a.overlap(&d), None); // adjacent = disjoint (half-open)
    }

    #[test]
    fn time_window_contains() {
        let w = TimeWindow::new(10, 50);
        assert!(w.contains(10));
        assert!(w.contains(49));
        assert!(!w.contains(50)); // half-open
        assert!(!w.contains(9));
    }

    #[test]
    fn wait_type_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&WaitType::FutexWait).unwrap(),
            "\"futex_wait\""
        );
        assert_eq!(
            serde_json::to_string(&WaitType::FutexLockPi).unwrap(),
            "\"futex_lock_pi\""
        );
        assert_eq!(
            serde_json::to_string(&WaitType::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn edge_weight_with_wait_type() {
        let ew = EdgeWeight::with_wait_type(TimeWindow::new(0, 100), WaitType::FutexWait);
        assert_eq!(ew.wait_type, Some(WaitType::FutexWait));
        assert_eq!(ew.raw_wait_ms, 100);
    }

    #[test]
    fn edge_weight_default_none() {
        let ew = EdgeWeight::new(TimeWindow::new(0, 50));
        assert_eq!(ew.wait_type, None);
    }

    #[test]
    fn thread_id_display() {
        assert_eq!(format!("{}", ThreadId(-4)), "NIC");
        assert_eq!(format!("{}", ThreadId(-5)), "Disk");
        assert_eq!(format!("{}", ThreadId(-15)), "HardIRQ");
        assert_eq!(format!("{}", ThreadId(-16)), "SoftIRQ");
        assert_eq!(format!("{}", ThreadId(1234)), "T1234");
    }
}
