//! Cascade-cycle probe — mixed User-chain + IO 2-cycle graph
//! ===========================================================
//!
//! Per-Adrian-request diagnostic: before any spec amendment, verify
//! that the proposed cascade-terminal + inbound-filter fix behaves
//! correctly in a MIXED workload (not just pure IO fixtures).
//!
//! Topology modeled (closest paper-like pattern for an IO-bound wait):
//!
//!     T100 ────[0, 10]────> T200 ←───[0, 10]─── DISK
//!                             │                   ↑
//!                             └───[0, 10]─────────┘
//!                                (ADR-009 synthetic pair)
//!
//! Semantics: T100 (User) is blocked for 10ms waiting on T200 (Holder),
//! which is itself doing synchronous I/O to Disk during the entire
//! window. The ADR-009 User↔Disk pair is between T200 and Disk — the
//! outer User→Holder wait is a normal scheduler edge, not synthetic.
//!
//! Expected intuition (paper-faithful):
//!   T100→T200: 0   (T200 isn't at fault — it's itself waiting on Disk)
//!   T200→DISK: 10  (Disk is the root cause for the full 10ms)
//!   DISK→T200: 0   (return-edge bookkeeping, no semantic wait)
//!   inbound-to-pseudo ratio = 10/10 = 1.0 ≥ 0.70 ✓
//!
//! This test records what the CURRENT (pre-fix) cascade produces so we
//! have a before/after comparison in the thread discussion. The values
//! asserted below are the OBSERVED pre-fix numbers, documenting the
//! drift between code and spec §7.3. If the fix lands, this test
//! **will fail** — that's the intended signal.

use wperf::cascade::engine::cascade_engine;
use wperf::graph::types::{DISK_TID, NodeKind, ThreadId, TimeWindow, WaitType};
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

    // Synthetic IO edges (ADR-009) — both directions covering same window.
    g.add_edge_with_wait_type(holder, disk, TimeWindow::new(0, 10), WaitType::IoBlock);
    g.add_edge_with_wait_type(disk, holder, TimeWindow::new(0, 10), WaitType::IoBlock);

    g
}

#[test]
#[allow(clippy::similar_names)] // user_holder / holder_disk / disk_holder are intentional
fn probe_mixed_cascade_current_behavior() {
    let g = build_mixed_graph();
    let result = cascade_engine(&g, None).expect("cascade must succeed");

    let user = ThreadId(100);
    let holder = ThreadId(200);
    let disk = ThreadId(DISK_TID);

    // Dump every edge for the thread discussion.
    println!("\n=== probe_mixed_cascade_current_behavior ===");
    for (_, src, dst, weight) in result.all_edges() {
        println!(
            "  {:>4} → {:<4}  raw={:>3}  attributed={:>3}  wait_type={:?}",
            src.0, dst.0, weight.raw_wait_ms, weight.attributed_delay_ms, weight.wait_type,
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

    // Observed pre-fix attribution — see docstring for paper-intent expected
    // values.
    let attr_uh = user_holder.3.attributed_delay_ms;
    let attr_hd = holder_disk.3.attributed_delay_ms;
    let attr_dh = disk_holder.3.attributed_delay_ms;

    println!("pre-fix attribution:");
    println!("  T100→T200 (real user wait): {attr_uh}  (paper-intent: 0)");
    println!("  T200→DISK (IO initiation):  {attr_hd}  (paper-intent: 10)");
    println!("  DISK→T200 (return bookkeeping): {attr_dh}  (paper-intent: 0)");

    // Pseudo-thread ratio candidates:
    let ratio_both = {
        let raw: u64 = holder_disk.3.raw_wait_ms + disk_holder.3.raw_wait_ms;
        let attr: u64 = attr_hd + attr_dh;
        #[allow(clippy::cast_precision_loss)]
        (attr as f64 / raw as f64)
    };
    let ratio_inbound = {
        let raw: u64 = holder_disk.3.raw_wait_ms;
        let attr: u64 = attr_hd;
        #[allow(clippy::cast_precision_loss)]
        (attr as f64 / raw as f64)
    };
    println!();
    println!("ratio (current impl, both dirs):   {ratio_both:.3}  (gate ≥ 0.70: FAIL if < 0.70)");
    println!("ratio (proposed inbound-only):     {ratio_inbound:.3}");
    println!();

    // Pre-fix assertions — these will BREAK when the fix lands, which is
    // how we detect the fix has taken effect.
    //
    // Intentionally uses concrete numbers so the thread can cite them.
    assert_eq!(
        attr_uh, 5,
        "pre-fix T100→T200 attribution (CURRENT buggy behavior — should be 0 post-fix)"
    );
    assert_eq!(
        attr_hd, 5,
        "pre-fix T200→DISK attribution (CURRENT buggy — should be 10 post-fix)"
    );
    assert_eq!(
        attr_dh, 0,
        "DISK→T200 return bookkeeping — already 0, fix preserves"
    );

    // Current-impl ratio does not meet the gate even on mixed workload.
    assert!(ratio_both < 0.70, "pre-fix both-dirs ratio is below gate");
    assert!(
        ratio_inbound < 0.70,
        "pre-fix inbound-only ratio ALSO below gate"
    );
}
