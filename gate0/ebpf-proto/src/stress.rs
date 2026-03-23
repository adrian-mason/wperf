//! Gate 0 Amendment 2: Buffer stress test
//!
//! Tests perf_event_array and ringbuf at various buffer sizes and poll intervals
//! under stress-ng load to determine optimal configuration for Phase 1.
//!
//! Run: cargo build && sudo ./target/debug/stress

mod probe {
    include!(concat!(env!("OUT_DIR"), "/probe.skel.rs"));
}
mod probe_rb {
    include!(concat!(env!("OUT_DIR"), "/probe_rb.skel.rs"));
}

use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::MapCore;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const COLLECT_SECS: u64 = 5;

fn run_perfbuf_test(pages: usize) -> (u64, u64, f64) {
    let builder = probe::ProbeSkelBuilder::default();
    let mut open_obj = MaybeUninit::uninit();
    let open = builder.open(&mut open_obj).unwrap();
    let mut skel = open.load().unwrap();
    skel.attach().unwrap();

    let event_count = Arc::new(AtomicU64::new(0));
    let lost_count = Arc::new(AtomicU64::new(0));
    let ec = event_count.clone();
    let lc = lost_count.clone();

    let perf = libbpf_rs::PerfBufferBuilder::new(&skel.maps.events)
        .pages(pages)
        .sample_cb(move |_cpu, _data: &[u8]| {
            ec.fetch_add(1, Ordering::Relaxed);
        })
        .lost_cb(move |_cpu, count| {
            lc.fetch_add(count as u64, Ordering::Relaxed);
        })
        .build()
        .unwrap();

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(COLLECT_SECS) {
        perf.poll(Duration::from_millis(100)).ok();
    }

    let events = event_count.load(Ordering::Relaxed);
    let lost = lost_count.load(Ordering::Relaxed);
    let rate = if events + lost > 0 {
        lost as f64 / (events + lost) as f64 * 100.0
    } else {
        0.0
    };
    (events, lost, rate)
}

fn run_ringbuf_test(buf_size_mb: usize, poll_timeout_ms: u64) -> (u64, u64, f64) {
    let builder = probe_rb::ProbeRbSkelBuilder::default();
    let mut open_obj = MaybeUninit::uninit();
    let mut open = builder.open(&mut open_obj).unwrap();

    // Override ringbuf size
    let size_bytes = buf_size_mb * 1024 * 1024;
    open.maps.events.set_max_entries(size_bytes as u32).unwrap();

    let mut skel = open.load().unwrap();
    skel.attach().unwrap();

    let event_count = Arc::new(AtomicU64::new(0));
    let ec = event_count.clone();

    let mut rb_builder = libbpf_rs::RingBufferBuilder::new();
    rb_builder.add(&skel.maps.events, move |_data: &[u8]| -> i32 {
        ec.fetch_add(1, Ordering::Relaxed);
        0
    }).unwrap();
    let rb = rb_builder.build().unwrap();

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(COLLECT_SECS) {
        rb.poll(Duration::from_millis(poll_timeout_ms)).ok();
    }

    let events = event_count.load(Ordering::Relaxed);
    // Read drop_count from BPF .bss map
    let drops = read_bss_drop_count(&skel);
    let rate = if events + drops > 0 {
        drops as f64 / (events + drops) as f64 * 100.0
    } else {
        0.0
    };
    (events, drops, rate)
}

fn main() {
    println!("============================================================");
    println!("Gate 0: Buffer Stress Test");
    println!("Host: {} CPUs, collecting {}s per test", num_cpus(), COLLECT_SECS);
    println!("============================================================\n");

    println!("--- Phase 1: Buffer Size Sweep (poll=100ms) ---\n");
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10}",
        "Test", "Type", "Size", "Events", "Drops", "Drop%");
    println!("{}", "-".repeat(72));

    // Test A: perfbuf 64 pages (256KB/CPU)
    let (e, d, r) = run_perfbuf_test(64);
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10.4}",
        "A", "perf_event_array", "256KB/CPU", e, d, r);

    // Test B: perfbuf 256 pages (1MB/CPU)
    let (e, d, r) = run_perfbuf_test(256);
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10.4}",
        "B", "perf_event_array", "1MB/CPU", e, d, r);

    // Test C: ringbuf 8MB
    let (e, d, r) = run_ringbuf_test(8, 100);
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10.4}",
        "C", "ringbuf", "8MB", e, d, r);

    // Test D: ringbuf 32MB
    let (e, d, r) = run_ringbuf_test(32, 100);
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10.4}",
        "D", "ringbuf", "32MB", e, d, r);

    println!("\n--- Phase 2: Poll Interval Sweep (ringbuf 32MB) ---\n");
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10}",
        "Test", "Poll Timeout", "Size", "Events", "Drops", "Drop%");
    println!("{}", "-".repeat(72));

    // Test E: 10ms poll
    let (e, d, r) = run_ringbuf_test(32, 10);
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10.4}",
        "E", "10ms", "32MB", e, d, r);

    // Test F: 100ms poll (same as D, repeated for consistency)
    let (e, d, r) = run_ringbuf_test(32, 100);
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10.4}",
        "F", "100ms", "32MB", e, d, r);

    // Test G: 500ms poll
    let (e, d, r) = run_ringbuf_test(32, 500);
    println!("{:<6} {:<20} {:<12} {:<12} {:<12} {:<10.4}",
        "G", "500ms", "32MB", e, d, r);

    println!("\n============================================================");
    println!("Done. Use results to determine Phase 1 buffer configuration.");
    println!("============================================================");
}

fn read_bss_drop_count(skel: &probe_rb::ProbeRbSkel) -> u64 {
    // The .bss section is exposed as a map. Find it and read drop_count (first 8 bytes).
    for m in skel.object().maps() {
        if m.name().to_string_lossy().contains("bss") {
            let key = 0u32.to_le_bytes();
            if let Ok(Some(val)) = m.lookup(&key, libbpf_rs::MapFlags::ANY) {
                if val.len() >= 8 {
                    return u64::from_le_bytes(val[..8].try_into().unwrap());
                }
            }
        }
    }
    0
}

fn num_cpus() -> usize {
    std::fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .matches("processor")
        .count()
}
