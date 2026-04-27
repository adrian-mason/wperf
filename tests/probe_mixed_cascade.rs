//! Cascade-cycle post-fix verification — mixed User-chain + IO 2-cycle graph
//! ===========================================================================
//!
//! Originally written as a "pre-fix red baseline" probe (PR #120 Challenger
//! Gap 1) to demonstrate the cascade-cycle bug on a mixed workload before
//! the ADR-009 *Amendment 2026-04-25* (PR #121, merged at `ae7a6b3`)
//! introduced `EdgeKind::SyntheticClosureReturn` and the bilateral edge
//! filter in `sweep_line_partition` + `count_concurrent_waiters`.
//!
//! Now the fix has landed. This file pivots from "pre-fix bug demo" to
//! "post-fix verification with empirically-pinned magnitudes":
//!
//! Topology:
//!
//!     T100 ────[0, 10]────> T200 ←───[0, 10]─── DISK
//!                             │                   ↑
//!                             └───[0, 10]─────────┘
//!                  forward (Normal)        return (SyntheticClosureReturn)
//!
//! Semantics: T100 (User) is blocked for 10ms waiting on T200 (Holder),
//! which is itself doing synchronous I/O to Disk during the entire window.
//! The return edge is constructed via `add_synthetic_closure_return` so
//! the cascade engine's bilateral filter (`sweep_line_partition` +
//! `count_concurrent_waiters`) treats it as cascade-terminal per ADR-009
//! Amendment.
//!
//! Expected post-fix attribution (paper-faithful):
//!   T100→T200: 0   (T200 isn't at fault — it's itself waiting on Disk)
//!   T200→DISK: 10  (Disk is the root cause for the full 10ms)
//!   DISK→T200: 0   (return-edge bookkeeping, no semantic wait)
//!   inbound-to-pseudo ratio = 10/10 = 1.0 ≥ 0.70 ✓
//!
//! These pinned values are now load-bearing: the ADR-009 Amendment
//! frontmatter (Verification Provenance) requires re-justification at
//! the multi-reviewer hand-trace level if a future change shifts these
//! numbers. They are not free parameters.

use wperf::cascade::engine::cascade_engine;
use wperf::graph::types::{DISK_TID, EdgeKind, NodeKind, ThreadId, TimeWindow, WaitType};
use wperf::graph::wfg::WaitForGraph;

fn build_mixed_graph() -> WaitForGraph {
    let mut g = WaitForGraph::new();
    let user = ThreadId(100);
    let holder = ThreadId(200);
    let disk = ThreadId(DISK_TID);

    g.add_node(user, NodeKind::UserThread);
    g.add_node(holder, NodeKind::UserThread);
    g.add_node(disk, NodeKind::PseudoDisk);

    // Real scheduler-derived wait: T100 blocked on T200 for [0, 10]ms.
    g.add_edge(user, holder, TimeWindow::new(0, 10));

    // Forward synthetic IO edge (User→Disk issue event) — Normal kind,
    // participates in cascade as a real wait dependency.
    g.add_edge_with_wait_type(holder, disk, TimeWindow::new(0, 10), WaitType::IoBlock);

    // Return synthetic IO edge (Disk→User completion event) —
    // SyntheticClosureReturn kind per ADR-009 Amendment 2026-04-25.
    // Cascade-terminal: skipped in sweep_line_partition + count_concurrent_waiters.
    g.add_synthetic_closure_return(disk, holder, TimeWindow::new(0, 10), WaitType::IoBlock);

    g
}

#[test]
#[allow(clippy::similar_names)] // user_holder / holder_disk / disk_holder are intentional
fn probe_mixed_cascade_post_fix_attribution() {
    let g = build_mixed_graph();
    let result = cascade_engine(&g, None).expect("cascade must succeed");

    let user = ThreadId(100);
    let holder = ThreadId(200);
    let disk = ThreadId(DISK_TID);

    // Dump every edge for documentation.
    println!("\n=== probe_mixed_cascade_post_fix_attribution ===");
    for (_, src, dst, weight) in result.all_edges() {
        println!(
            "  {:>4} → {:<4}  raw={:>3}  attributed={:>3}  wait_type={:?}  kind={:?}",
            src.0,
            dst.0,
            weight.raw_wait_ms,
            weight.attributed_delay_ms,
            weight.wait_type,
            weight.kind,
        );
    }
    println!();

    let edges = result.all_edges();
    let user_holder = edges
        .iter()
        .find(|(_, s, d, _)| *s == user && *d == holder)
        .expect("T100→T200 edge present");
    let holder_disk = edges
        .iter()
        .find(|(_, s, d, _)| *s == holder && *d == disk)
        .expect("T200→DISK edge present");
    let disk_holder = edges
        .iter()
        .find(|(_, s, d, _)| *s == disk && *d == holder)
        .expect("DISK→T200 edge present");

    let attr_uh = user_holder.3.attributed_delay_ms;
    let attr_hd = holder_disk.3.attributed_delay_ms;
    let attr_dh = disk_holder.3.attributed_delay_ms;

    println!("post-fix attribution:");
    println!("  T100→T200 (real user wait): {attr_uh}  (paper-intent: 0)");
    println!("  T200→DISK (IO initiation):  {attr_hd}  (paper-intent: 10)");
    println!("  DISK→T200 (return bookkeeping): {attr_dh}  (paper-intent: 0)");

    // Inbound-to-pseudo ratio (per §7.3 formal definition: e.dst == DISK
    // AND e.kind != SyntheticClosureReturn — only T200→DISK qualifies).
    let inbound_raw: u64 = holder_disk.3.raw_wait_ms;
    let inbound_attr: u64 = attr_hd;
    #[allow(clippy::cast_precision_loss)]
    let ratio_inbound = inbound_attr as f64 / inbound_raw as f64;
    println!("ratio (inbound-only, §7.3 formal): {ratio_inbound:.3}  (gate ≥ 0.70)");

    // Pinned post-fix expectations — these are load-bearing per ADR-009
    // Amendment Verification Provenance. Future modifications require
    // re-justification at multi-reviewer hand-trace level.
    assert_eq!(
        attr_uh, 0,
        "post-fix T100→T200 must attribute 0 (T200 is victim, all blame cascades to DISK)"
    );
    assert_eq!(
        attr_hd, 10,
        "post-fix T200→DISK must attribute 10 (DISK is root cause, full attribution)"
    );
    assert_eq!(
        attr_dh, 0,
        "post-fix DISK→T200 (SyntheticClosureReturn) must remain 0 — closure bookkeeping retains no semantic blame"
    );
    assert!(
        (ratio_inbound - 1.0).abs() < 1e-9,
        "post-fix inbound-only ratio must be 1.0, got {ratio_inbound}"
    );

    // Edge-kind invariants — closure-return edge MUST carry the marker;
    // forward edges MUST be Normal. Drift on either invariant would
    // silently re-enable the pre-fix bug.
    assert_eq!(
        disk_holder.3.kind,
        EdgeKind::SyntheticClosureReturn,
        "DISK→T200 must be marked SyntheticClosureReturn"
    );
    assert_eq!(
        holder_disk.3.kind,
        EdgeKind::Normal,
        "T200→DISK forward edge must be Normal"
    );
    assert_eq!(
        user_holder.3.kind,
        EdgeKind::Normal,
        "T100→T200 user-wait edge must be Normal"
    );
}

/// 3-node Knot fixture per Probe / Critic / Challenger requirement
/// (PR #121 thread: Probe Gap 2, Challenger msg=296b39ab Gap 2). The
/// 2-cycle test above only validates cascade attribution; this fixture
/// exercises the SCC + critical-path DP path with a pseudo-thread inside
/// the SCC, ensuring the bilateral edge-filter doesn't accidentally
/// regress Knot detection or DP behavior on graphs that contain a real
/// SCC closure plus an upstream entry edge.
///
/// Topology:
///
///     T1 ────[0, 10]────> T2 ←───[0, 10]─── DISK
///                          │                  ↑
///                          └───[0, 10]────────┘
///                  forward (Normal)    return (SyntheticClosureReturn)
///
/// - {T2, DISK} forms an SCC (T2 → DISK forward + DISK → T2 closure).
/// - T1 → T2 is the SCC entry edge.
/// - The SCC is a sink in the condensation DAG (no outgoing from {T2, DISK}
///   to anything outside the SCC), so it qualifies as a Knot per ADR-008
///   if it contains ≥1 user thread (T2 is a `UserThread`).
///
/// Post-fix expected:
///   T1→T2:   attributed=0  (T2 is victim, all blame cascades to DISK)
///   T2→DISK: attributed=10 (DISK root cause)
///   DISK→T2: attributed=0  (closure-return bookkeeping)
///   The SCC {T2, DISK} is detected as a Knot containing a `UserThread`
///   (T2). Critical-path DP super-node weight per ADR-008 MAX heuristic
///   = max(attributed across internal edges) = max(10, 0) = 10.
#[test]
fn probe_3node_knot_with_pseudo_thread() {
    use wperf::scc::knot::detect_knots;
    use wperf::scc::tarjan::build_condensation;

    let mut g = WaitForGraph::new();
    let t1 = ThreadId(1);
    let t2 = ThreadId(2);
    let disk = ThreadId(DISK_TID);

    g.add_node(t1, NodeKind::UserThread);
    g.add_node(t2, NodeKind::UserThread);
    g.add_node(disk, NodeKind::PseudoDisk);

    g.add_edge(t1, t2, TimeWindow::new(0, 10));
    g.add_edge_with_wait_type(t2, disk, TimeWindow::new(0, 10), WaitType::IoBlock);
    g.add_synthetic_closure_return(disk, t2, TimeWindow::new(0, 10), WaitType::IoBlock);

    let cascaded = cascade_engine(&g, None).expect("cascade must succeed");

    // Cascade attribution post-fix.
    let edges = cascaded.all_edges();
    let attr = |s: ThreadId, d: ThreadId| -> u64 {
        match edges.iter().find(|(_, src, dst, _)| *src == s && *dst == d) {
            Some((_, _, _, w)) => w.attributed_delay_ms,
            None => panic!("edge {}→{} not found", s.0, d.0),
        }
    };
    assert_eq!(
        attr(t1, t2),
        0,
        "T1→T2: T2 is victim, blame cascades to DISK"
    );
    assert_eq!(attr(t2, disk), 10, "T2→DISK: full attribution");
    assert_eq!(
        attr(disk, t2),
        0,
        "DISK→T2 (SyntheticClosureReturn): closure bookkeeping retains no blame"
    );

    // Tarjan SCC detection — the {T2, DISK} pair must form an SCC because
    // both edges (forward + return) participate in adjacency analysis,
    // even though the return edge is cascade-terminal.
    let cdag = build_condensation(&cascaded);
    let knots = detect_knots(&cdag, &cascaded);

    // The {T2, DISK} SCC must be detected as a Knot:
    // - It is a sink in the condensation DAG (no edges out)
    // - It contains a UserThread (T2)
    let knot_with_t2 = knots.iter().find(|k| k.members.contains(&t2));
    assert!(
        knot_with_t2.is_some(),
        "{{T2, DISK}} SCC must be detected as Knot (sink + contains UserThread T2); got knots: {knots:?}"
    );
    let knot = knot_with_t2.unwrap();
    assert!(
        knot.members.contains(&disk),
        "Knot must contain DISK pseudo-thread: {knot:?}"
    );
    assert_eq!(knot.members.len(), 2, "Knot is exactly {{T2, DISK}}");
}

#[test]
fn probe_mixed_cascade_filter_must_apply_bilaterally() {
    // Defense-in-depth check: if a future regression filters the SCR
    // edge in sweep but not in count_concurrent_waiters (or vice versa),
    // the divisor stays polluted and forward-edge attribution silently
    // halves. This test would catch that — currently both filters are
    // correctly applied so attribution is 10 (full); a single-side
    // regression would push it down to 5 (the pre-fix value), which the
    // strict equality assertion above also catches.
    //
    // We add a separate fixture here that adds an extra Normal incoming
    // edge to T200 (a third user T300 also waiting on T200). With the
    // bilateral filter correctly applied, count_concurrent_waiters(T200)
    // = 2 (T100 + T300), not 3 (T100 + T300 + DISK-via-SCR). The forward
    // edge T200→DISK still gets attribution = 10 because Disk's own sweep
    // is empty (no Normal outgoing).

    let mut g = WaitForGraph::new();
    let t100 = ThreadId(100);
    let t300 = ThreadId(300);
    let holder = ThreadId(200);
    let disk = ThreadId(DISK_TID);

    g.add_node(t100, NodeKind::UserThread);
    g.add_node(t300, NodeKind::UserThread);
    g.add_node(holder, NodeKind::UserThread);
    g.add_node(disk, NodeKind::PseudoDisk);

    g.add_edge(t100, holder, TimeWindow::new(0, 10));
    g.add_edge(t300, holder, TimeWindow::new(0, 10));
    g.add_edge_with_wait_type(holder, disk, TimeWindow::new(0, 10), WaitType::IoBlock);
    g.add_synthetic_closure_return(disk, holder, TimeWindow::new(0, 10), WaitType::IoBlock);

    let result = cascade_engine(&g, None).expect("cascade must succeed");

    let edges = result.all_edges();
    let holder_disk = edges
        .iter()
        .find(|(_, s, d, _)| *s == holder && *d == disk)
        .expect("T200→DISK edge present");

    // T200→DISK forward attribution must still be 10 — the third user
    // T300 doesn't change this because Disk is a leaf for cascade
    // (its only outgoing is the SCR edge, which is filtered).
    assert_eq!(
        holder_disk.3.attributed_delay_ms, 10,
        "T200→DISK forward attribution unaffected by additional concurrent waiters on T200"
    );
}
