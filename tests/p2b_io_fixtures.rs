//! Phase 2b #38 P2b-01 commit-6 — end-to-end block-IO fixture suite.
//!
//! Four named fixtures cover the behavioral matrix for `block_rq` synthetic
//! edges: `cpu_only_baseline` (no I/O — regression guard that new counters
//! stay inert), `fio_randread_direct` (single submitter, many paired I/Os),
//! `mixed_cpu_io_futex` (I/O interleaved with `sched_switch` + futex), and
//! `concurrent_submitters_single_disk` (fan-in to the single `DISK_TID`).
//!
//! Plus two sanity fixtures: `io_health_counters_surface_real_values`
//! exercises the orphan + pair-collision counters, and
//! `all_io_edges_annotated_with_ioblock_wait_type` guards `WaitType::IoBlock`
//! annotation across cascade.
//!
//! Each fixture asserts `invariants_ok`, edge topology, and counter values;
//! where meaningful, the attributed-delay ratio is checked for well-definedness
//! (value depends on cascade semantics for closed cycles — see per-test docs).
//! The proptest in `tests/property_tests.rs` covers the fuzz surface; these
//! fixtures are the canonical named-scenario acceptance tests.

use std::io::Cursor;

use wperf::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS;
use wperf::format::event::{EventType, WperfEvent};
use wperf::format::reader::WperfReader;
use wperf::format::writer::WperfWriter;
use wperf::graph::types::{DISK_TID, ThreadId, WaitType};
use wperf::report::{self, ReportOutput};

const TASK_INTERRUPTIBLE: u8 = 1;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_report_from(events: &[WperfEvent]) -> ReportOutput {
    let buf = Cursor::new(Vec::new());
    let mut w = WperfWriter::new(buf).expect("writer");
    for ev in events {
        w.write_event(ev).expect("write_event");
    }
    let data = w.finish(0).expect("finish").into_inner();
    let mut r = WperfReader::open(Cursor::new(data)).expect("reader");
    report::build_report(&mut r, DEFAULT_SPURIOUS_THRESHOLD_NS).expect("build_report")
}

fn switch(ts_ns: u64, prev_tid: u32, next_tid: u32, prev_state: u8) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts_ns,
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

#[allow(clippy::similar_names)]
fn wakeup(ts_ns: u64, waker_tid: u32, wakee_tid: u32) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts_ns,
        pid: 0,
        tid: 0,
        prev_tid: waker_tid,
        next_tid: wakee_tid,
        prev_pid: 0,
        next_pid: 0,
        cpu: 0,
        event_type: EventType::Wakeup as u8,
        prev_state: 0,
        flags: 0,
    }
}

#[allow(clippy::similar_names)] // tgid / tid mirror WperfEvent field names
fn io_event(
    ts_ns: u64,
    tgid: u32,
    tid: u32,
    dev: u32,
    sector: u64,
    nr_sector: u32,
    kind: EventType,
) -> WperfEvent {
    WperfEvent {
        timestamp_ns: ts_ns,
        pid: tgid,
        tid,
        prev_tid: u32::try_from(sector & 0xFFFF_FFFF).unwrap(),
        next_tid: u32::try_from(sector >> 32).unwrap(),
        prev_pid: dev,
        next_pid: nr_sector,
        cpu: 0,
        event_type: kind as u8,
        prev_state: 0,
        flags: 0,
    }
}

// ---------------------------------------------------------------------------
// Fixture 1 — cpu_only_baseline
// ---------------------------------------------------------------------------

#[test]
fn cpu_only_baseline() {
    // Two threads, a classic User→Holder wait chain. No IO events at all.
    // Regression guard: the new io_* counters must all be 0 and
    // attributed_delay_ratio must be None (no IoBlock edges present).
    let events = vec![
        switch(1_000_000, 100, 200, TASK_INTERRUPTIBLE), // T100 off
        wakeup(2_000_000, 200, 100),                     // T200 wakes T100
        switch(3_000_000, 200, 100, 0),                  // T100 back on
    ];

    let report = build_report_from(&events);

    assert_eq!(report.cascade.edges.len(), 1, "one user-wait edge");
    assert!(report.health.invariants_ok);
    // No IO — all IO health fields must be the zero-default (tracing ran but
    // saw nothing), except the ratio which is None (undefined without edges).
    assert_eq!(report.health.io_orphan_complete_count, Some(0));
    assert_eq!(report.health.io_pending_at_end_count, Some(0));
    assert_eq!(report.health.io_userspace_pair_collision_count, Some(0));
    assert_eq!(report.health.attributed_delay_ratio, None);
}

// ---------------------------------------------------------------------------
// Fixture 2 — fio_randread_direct
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::similar_names)]
fn fio_randread_direct() {
    // Single submitter T100 issues a burst of 8 paired I/Os to one device,
    // each with distinct sectors. Every issue gets its matching complete;
    // no orphans, no dangles, no collisions.
    let tgid = 500u32;
    let tid = 500u32;
    let dev = 0x800_0001;
    let mut events = Vec::new();
    for i in 0..8u64 {
        let sector = 0x1000 + i * 0x100;
        let issue_ts = 1_000_000 + i * 10_000_000; // 1ms apart
        let complete_ts = issue_ts + 5_000_000; // 5ms service time
        events.push(io_event(
            issue_ts,
            tgid,
            tid,
            dev,
            sector,
            8,
            EventType::IoIssue,
        ));
        events.push(io_event(
            complete_ts,
            tgid,
            tid,
            dev,
            sector,
            8,
            EventType::IoComplete,
        ));
    }

    let report = build_report_from(&events);

    // 8 I/Os × 2 edges = 16 IoBlock edges; nodes = 1 user + 1 DISK.
    let user = ThreadId(i64::from(tid));
    let disk = ThreadId(DISK_TID);
    let io_edges: Vec<_> = report
        .cascade
        .edges
        .iter()
        .filter(|e| (e.src == user && e.dst == disk) || (e.src == disk && e.dst == user))
        .collect();
    assert_eq!(io_edges.len(), 16, "8 pairs × 2 directions");

    assert!(report.health.invariants_ok);
    assert_eq!(report.health.io_orphan_complete_count, Some(0));
    assert_eq!(report.health.io_pending_at_end_count, Some(0));
    assert_eq!(report.health.io_userspace_pair_collision_count, Some(0));

    // Post-PR-#121 cascade-terminal fix + post-commit-10 §7.3 per-P ratio:
    // forward edges (User→Disk, kind=Normal) attribute to DISK at full
    // raw_wait under cascade (pseudo-thread is now a leaf for cascade —
    // its only outgoing is the SCR edge which the bilateral filter
    // skips). Return edges (kind=SyntheticClosureReturn) are excluded
    // from §7.3 numerator + denominator. So for 8 paired I/Os each with
    // raw_wait=5ms (5_000_000ns / 1_000_000), the per-P "disk" ratio =
    // 40/40 = 1.0.
    //
    // STRICT magnitude assertion per spec §7.3 L588 defense-in-depth +
    // per-P discipline: every observed pseudo-thread MUST be in
    // [0.70, 1.0], and (b) hard-precondition demands at least one
    // defined entry. 5-way reviewer convergence on per-P contract:
    // Oracle msg=62ec6c36 + Probe msg=a1f9b1bf + Critic msg=5bcd369d +
    // Challenger msg=a21b9b2b + Maestro msg=54b49254.
    let ratio_map = report
        .health
        .attributed_delay_ratio
        .as_ref()
        .expect("inbound-to-pseudo-thread edges present → per-P map must be Some(_)");
    assert!(
        !ratio_map.is_empty(),
        "(b) hard-precondition: ≥1 IO pseudo-thread must have a defined ratio"
    );
    for (label, &r) in ratio_map {
        assert!(
            (0.70..=1.0).contains(&r),
            "per-P {label} ratio must be in [0.70, 1.0] per spec §7.3 (a), got {r}"
        );
    }
}

// ---------------------------------------------------------------------------
// Fixture 3 — mixed_cpu_io_futex
// ---------------------------------------------------------------------------

#[test]
fn mixed_cpu_io_futex() {
    // Heterogeneous workload: two threads that both sched_switch + issue I/O
    // interleaved. Proves the correlate pipeline handles mixed event streams
    // without cross-contamination (futex event for T100 must not contaminate
    // T200's IO edge).
    let events = vec![
        // T100: goes off-CPU waiting on T200
        switch(1_000_000, 100, 200, TASK_INTERRUPTIBLE),
        // T200 issues + completes I/O while T100 is parked
        io_event(
            2_000_000,
            200,
            200,
            0x800_0001,
            0x2000,
            16,
            EventType::IoIssue,
        ),
        io_event(
            5_000_000,
            200,
            200,
            0x800_0001,
            0x2000,
            16,
            EventType::IoComplete,
        ),
        // T200 wakes T100
        wakeup(6_000_000, 200, 100),
        switch(7_000_000, 200, 100, 0),
    ];

    let report = build_report_from(&events);

    // Expect edges: User→Holder (T100→T200), plus User↔Disk (T200↔DISK).
    assert!(report.health.invariants_ok);
    assert_eq!(report.health.io_orphan_complete_count, Some(0));
    assert_eq!(report.health.io_pending_at_end_count, Some(0));
    assert_eq!(report.health.io_userspace_pair_collision_count, Some(0));

    let t200 = ThreadId(200);
    let t100 = ThreadId(100);
    let disk = ThreadId(DISK_TID);
    let io_edge_count = report
        .cascade
        .edges
        .iter()
        .filter(|e| (e.src == t200 && e.dst == disk) || (e.src == disk && e.dst == t200))
        .count();
    assert_eq!(io_edge_count, 2, "one User↔Disk pair");

    let user_wait_edge = report
        .cascade
        .edges
        .iter()
        .find(|e| e.src == t100 && e.dst == t200);
    assert!(user_wait_edge.is_some(), "T100→T200 wait edge preserved");
}

// ---------------------------------------------------------------------------
// Fixture 4 — concurrent_submitters_single_disk
// ---------------------------------------------------------------------------

#[test]
fn concurrent_submitters_single_disk() {
    // Three user threads hammer the same device concurrently. The single
    // DISK_TID pseudo-thread receives inbound edges from each user and
    // outbound return edges to each. This is the fan-in pattern that
    // ADR-009 §Consequences flags as "per-device pseudo-disks deferred".
    let dev = 0x800_0001;
    let mut events = Vec::new();
    for (i, tid) in [301u32, 302, 303].iter().enumerate() {
        let sector = 0x1000 + (i as u64) * 0x100;
        let issue_ts = 1_000_000 + (i as u64) * 100_000;
        let complete_ts = issue_ts + 3_000_000;
        events.push(io_event(
            issue_ts,
            *tid,
            *tid,
            dev,
            sector,
            8,
            EventType::IoIssue,
        ));
        events.push(io_event(
            complete_ts,
            *tid,
            *tid,
            dev,
            sector,
            8,
            EventType::IoComplete,
        ));
    }

    let report = build_report_from(&events);

    assert!(report.health.invariants_ok);
    assert_eq!(report.health.io_orphan_complete_count, Some(0));
    assert_eq!(report.health.io_pending_at_end_count, Some(0));
    assert_eq!(report.health.io_userspace_pair_collision_count, Some(0));

    // Each user tid contributes 2 IoBlock edges → 6 IO edges total.
    let user_tids = [ThreadId(301), ThreadId(302), ThreadId(303)];
    let disk = ThreadId(DISK_TID);
    let io_edges = report
        .cascade
        .edges
        .iter()
        .filter(|e| {
            user_tids.contains(&e.src) && e.dst == disk
                || e.src == disk && user_tids.contains(&e.dst)
        })
        .count();
    assert_eq!(io_edges, 6, "3 submitters × 2 directions");

    // Post-PR-#121 cascade-terminal fix + post-commit-10 §7.3 per-P ratio:
    // forward edges (User→Disk per submitter, kind=Normal) attribute to
    // DISK at full raw_wait under cascade. Return edges (kind=SCR) excluded
    // from §7.3 numerator + denominator. With 3 submitters × 1 IO each
    // (3ms service time), per-P "disk" raw_sum = 9, attributed_sum = 9,
    // ratio = 1.0.
    //
    // STRICT magnitude assertion per spec §7.3 L588 — forward fan-in to a
    // single pseudo-thread should attribute correctly even when multiple
    // submitters share the same DISK target (Probe Gap 5 verifies the
    // `count_concurrent_waiters` filter doesn't pollute fan-in). Per-P
    // ∀ check + (b) hard-precondition per 5-way reviewer convergence
    // (Oracle msg=62ec6c36 et al.).
    let ratio_map = report
        .health
        .attributed_delay_ratio
        .as_ref()
        .expect("inbound-to-pseudo-thread edges present → per-P map must be Some(_)");
    assert!(
        !ratio_map.is_empty(),
        "(b) hard-precondition: ≥1 IO pseudo-thread must have a defined ratio"
    );
    for (label, &r) in ratio_map {
        assert!(
            (0.70..=1.0).contains(&r),
            "per-P {label} ratio must be in [0.70, 1.0] per spec §7.3 (a), got {r}"
        );
    }
}

// ---------------------------------------------------------------------------
// Negative fixture — orphan + collision health counters populate
// ---------------------------------------------------------------------------

#[test]
fn io_health_counters_surface_real_values() {
    // One orphan complete (no prior issue), one pair collision (two issues
    // on the same IoKey before any complete). Verifies the HealthMetrics
    // surface actually wires to the underlying counters — not just zeros.
    let dev = 0x800_0001;
    let sector = 0x4000;
    let events = vec![
        // Orphan: complete with no prior issue.
        io_event(1_000_000, 111, 111, dev, 0x1000, 8, EventType::IoComplete),
        // Collision: two issues with identical (dev, sector, nr_sector).
        io_event(2_000_000, 222, 222, dev, sector, 8, EventType::IoIssue),
        io_event(3_000_000, 333, 333, dev, sector, 8, EventType::IoIssue),
        // Drain the collided issue so only the pair-collision counter fires.
        io_event(4_000_000, 333, 333, dev, sector, 8, EventType::IoComplete),
    ];

    let report = build_report_from(&events);

    assert!(report.health.invariants_ok);
    assert_eq!(
        report.health.io_orphan_complete_count,
        Some(1),
        "one orphan"
    );
    assert_eq!(report.health.io_pending_at_end_count, Some(0));
    assert_eq!(
        report.health.io_userspace_pair_collision_count,
        Some(1),
        "second identical issue must register as collision"
    );

    // Match the ADR-009 bidirectional edge rule: one successful pair = 2 edges.
    // Edges attribute to T333 (last-writer-wins on collision).
    let t333 = ThreadId(333);
    let disk = ThreadId(DISK_TID);
    let io_edges = report
        .cascade
        .edges
        .iter()
        .filter(|e| (e.src == t333 && e.dst == disk) || (e.src == disk && e.dst == t333))
        .count();
    assert_eq!(io_edges, 2);
}

// ---------------------------------------------------------------------------
// WaitType annotation — every IoBlock edge must be annotated
// ---------------------------------------------------------------------------

#[test]
fn all_io_edges_annotated_with_ioblock_wait_type() {
    // Structural guard: the CascadeResult serialization doesn't expose
    // wait_type directly (edges only show src/dst/raw/attributed), so we go
    // through `correlate_events` directly to inspect the WaitForGraph.
    use wperf::correlate::correlate_events;

    let events = vec![
        io_event(
            1_000_000,
            100,
            100,
            0x800_0001,
            0x1000,
            8,
            EventType::IoIssue,
        ),
        io_event(
            2_000_000,
            100,
            100,
            0x800_0001,
            0x1000,
            8,
            EventType::IoComplete,
        ),
    ];

    let (graph, _) = correlate_events(&events, 0);
    for (_, _, _, weight) in graph.all_edges() {
        assert_eq!(
            weight.wait_type,
            Some(WaitType::IoBlock),
            "every edge produced by IO dispatch must carry WaitType::IoBlock"
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 2b commit-9 magnitude-pinned multi-IO fixture
// (per spec final-design.md §7.3 L588 byte-encoded defense-in-depth)
// ---------------------------------------------------------------------------

/// Magnitude-pinned multi-IO fixture per spec final-design.md §7.3 L588:
///
/// > "commit-9 multi-IO fixtures (issue #38) MUST assert
/// >  `attributed_delay_ratio` is `Some(r) where 0.70 ≤ r ≤ 1.0` rather
/// >  than the looser `(0..=1.0)` range"
///
/// Probe risk-asymmetry argument (PR #121 thread, Probe msg=a8a05157 §2):
/// the spec gate is the system-level safety net (catches all-None /
/// cascade producing zero `raw_wait`), and the fixture-level magnitude
/// assert is the unit-test-level safety net (catches per-edge ratio
/// computation bugs that produce `Some(r)` with `r < 0.70` when the
/// real answer should be ~1.0). Both layers required per Oracle
/// msg=0c9bd48d §3 ratification of Probe §5(1) two-layer
/// defense-in-depth pattern.
///
/// Workload shape: 5 paired I/Os from a single submitter to one device,
/// each with 4ms service time (well above ns→ms truncation threshold).
/// Post-PR-#121 cascade: forward edges (Normal kind, dst=Disk) get full
/// attribution; return edges (SCR kind) excluded from §7.3 ratio.
/// Expected: ratio = 5×4 / 5×4 = 1.0 exactly.
#[test]
#[allow(clippy::similar_names)] // tgid / tid mirror WperfEvent field names
fn commit9_magnitude_pinned_multi_io_ratio() {
    use wperf::format::event::{EventType, WperfEvent};
    use wperf::format::reader::WperfReader;
    use wperf::format::writer::WperfWriter;
    use wperf::report::{self, ReportOutput};

    fn run(events: &[WperfEvent]) -> ReportOutput {
        let buf = std::io::Cursor::new(Vec::new());
        let mut w = WperfWriter::new(buf).expect("writer");
        for ev in events {
            w.write_event(ev).expect("write_event");
        }
        let data = w.finish(0).expect("finish").into_inner();
        let mut r = WperfReader::open(std::io::Cursor::new(data)).expect("reader");
        report::build_report(&mut r, wperf::correlate::DEFAULT_SPURIOUS_THRESHOLD_NS)
            .expect("build_report")
    }

    let tgid = 700u32;
    let tid = 700u32;
    let dev = 0x800_0007u32;
    let mut events: Vec<WperfEvent> = Vec::new();
    for i in 0..5u64 {
        let sector = 0x4000 + i * 0x100;
        let issue_ts = 1_000_000 + i * 10_000_000;
        let complete_ts = issue_ts + 4_000_000; // 4ms service time
        events.push(io_event(
            issue_ts,
            tgid,
            tid,
            dev,
            sector,
            8,
            EventType::IoIssue,
        ));
        events.push(io_event(
            complete_ts,
            tgid,
            tid,
            dev,
            sector,
            8,
            EventType::IoComplete,
        ));
    }

    let report = run(&events);

    assert!(report.health.invariants_ok);
    assert_eq!(report.health.io_orphan_complete_count, Some(0));
    assert_eq!(report.health.io_pending_at_end_count, Some(0));
    assert_eq!(report.health.io_userspace_pair_collision_count, Some(0));

    // STRICT pattern per Probe msg=a8a05157 §5(1) + Oracle msg=0c9bd48d
    // §3 ratify, plus per-P promotion per 5-way commit-10 convergence
    // (Oracle msg=62ec6c36 + Probe msg=a1f9b1bf + Critic msg=5bcd369d +
    // Challenger msg=a21b9b2b + Maestro msg=54b49254). Three layers:
    //   1. Outer Option must be Some (§7.3 (b) hard precondition);
    //      None means cascade or ratio impl regression.
    //   2. Per-P value ∀ in [0.70, 1.0] (§7.3 (a) gate predicate).
    //   3. The "disk" entry must be exactly 1.0 — clean pure-IO
    //      workload with no upstream user-wait chain forces full
    //      attribution to the pseudo-thread leaf (SCR filter).
    let ratio_map = report
        .health
        .attributed_delay_ratio
        .as_ref()
        .expect("inbound-to-pseudo-thread edges present → per-P map MUST be Some(_); None means cascade or ratio impl regression");
    assert!(
        !ratio_map.is_empty(),
        "(b) hard-precondition: ≥1 IO pseudo-thread must have a defined ratio"
    );
    for (label, &r) in ratio_map {
        assert!(
            (0.70..=1.0).contains(&r),
            "per-P {label} ratio must be in [0.70, 1.0] per spec §7.3 L588 defense-in-depth, got {r}"
        );
    }

    let disk = ratio_map
        .get("disk")
        .copied()
        .expect("DISK pseudo-thread must appear with this all-DISK workload");
    assert!(
        (disk - 1.0).abs() < 1e-9,
        "clean multi-IO workload MUST attribute exactly 1.0 to disk (no upstream cascade chains divert blame from forward IO edges); got {disk}"
    );
}
