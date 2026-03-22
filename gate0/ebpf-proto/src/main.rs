//! Gate 0 #7: Minimal eBPF collection prototype
//!
//! Hooks sched_switch + sched_wakeup via raw_tp, reads events via
//! perf_event_array, matches switch/wakeup pairs by TID.
//!
//! Run: cargo build && sudo ./target/debug/ebpf-proto
//! Throwaway code — discarded after Gate 0.

mod probe {
    include!(concat!(env!("OUT_DIR"), "/probe.skel.rs"));
}

use probe::*;
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const EVENT_SWITCH: u8 = 0;
const EVENT_WAKEUP: u8 = 1;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Event {
    r#type: u8,
    cpu: u16,
    prev_pid: u32,
    prev_tgid: u32,
    next_pid: u32,
    next_tgid: u32,
    timestamp_ns: u64,
    prev_state: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("============================================================");
    println!("Gate 0 #7: eBPF Minimal Collection Prototype");
    println!("============================================================\n");

    // Load and attach BPF
    let skel_builder = ProbeSkelBuilder::default();
    let mut open_object = MaybeUninit::uninit();
    let open_skel = skel_builder.open(&mut open_object)?;
    let mut skel = open_skel.load()?;
    skel.attach()?;
    println!("[OK] BPF loaded and attached (raw_tp/sched_switch + raw_tp/sched_wakeup)");

    // Spawn workload
    let (tid_a, tid_b) = spawn_mutex_workload();
    println!("[OK] Workload: TID_A={tid_a}, TID_B={tid_b}");
    println!("[..] Collecting 3 seconds...");

    // Collect events via perf buffer
    let collected: Arc<std::sync::Mutex<Vec<Event>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let c = collected.clone();

    let perf = libbpf_rs::PerfBufferBuilder::new(&skel.maps.events)
        .sample_cb(move |_cpu, data: &[u8]| {
            if data.len() >= std::mem::size_of::<Event>() {
                let event = unsafe { std::ptr::read_unaligned(data.as_ptr() as *const Event) };
                c.lock().unwrap().push(event);
            }
        })
        .lost_cb(|cpu, count| {
            eprintln!("[WARN] Lost {count} events on CPU {cpu}");
        })
        .build()?;

    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        perf.poll(Duration::from_millis(100))?;
    }

    let all = collected.lock().unwrap();
    println!("[OK] Total events: {}", all.len());

    // Filter for workload TIDs
    let switches: Vec<&Event> = all
        .iter()
        .filter(|e| {
            e.r#type == EVENT_SWITCH
                && (e.prev_pid == tid_a || e.prev_pid == tid_b
                    || e.next_pid == tid_a || e.next_pid == tid_b)
        })
        .collect();

    let wakeups: Vec<&Event> = all
        .iter()
        .filter(|e| {
            e.r#type == EVENT_WAKEUP
                && (e.next_pid == tid_a || e.next_pid == tid_b)
        })
        .collect();

    println!("[OK] Workload: {} switches, {} wakeups", switches.len(), wakeups.len());

    // Match wakeup → next switch by target TID
    let mut matched = 0u32;
    let mut sw_idx = 0;
    for wu in &wakeups {
        let target = wu.next_pid;
        for j in sw_idx..switches.len() {
            if switches[j].next_pid == target && switches[j].timestamp_ns >= wu.timestamp_ns {
                matched += 1;
                sw_idx = j + 1;
                break;
            }
        }
    }

    // Per-CPU monotonicity check (perf_event_array is per-CPU, no global ordering — ADR-004)
    let mut per_cpu_ts: std::collections::HashMap<u16, u64> = std::collections::HashMap::new();
    let mut per_cpu_monotonic = true;
    for e in all.iter() {
        let cpu = e.cpu;
        let ts = e.timestamp_ns;
        if let Some(prev) = per_cpu_ts.get(&cpu) {
            if ts < *prev {
                per_cpu_monotonic = false;
                break;
            }
        }
        per_cpu_ts.insert(cpu, ts);
    }

    // tgid check — find a workload event (not idle task)
    let tgid = switches
        .iter()
        .find(|e| e.next_pid == tid_a || e.next_pid == tid_b)
        .map(|e| e.next_tgid)
        .unwrap_or(0);

    // Sample output (copy fields from packed struct to avoid alignment issues)
    println!("\n--- Sample (first 3 switches) ---");
    for (i, e) in switches.iter().take(3).enumerate() {
        let (pp, pt, np, nt, ts) = (e.prev_pid, e.prev_tgid, e.next_pid, e.next_tgid, e.timestamp_ns);
        println!("  sw[{i}]: prev={pp}/{pt} next={np}/{nt} ts={ts}");
    }
    println!("--- Sample (first 3 wakeups) ---");
    for (i, e) in wakeups.iter().take(3).enumerate() {
        let (pp, pt, np, nt, ts) = (e.prev_pid, e.prev_tgid, e.next_pid, e.next_tgid, e.timestamp_ns);
        println!("  wu[{i}]: waker={pp}/{pt} target={np}/{nt} ts={ts}");
    }

    // Results
    println!("\n--- Validation ---");
    println!("  Matched:     {matched}/{}", wakeups.len());
    println!("  Orphans:     {}", wakeups.len() as u32 - matched);
    println!("  Per-CPU mono: {per_cpu_monotonic}");
    println!("  tgid:        {tgid}");

    let pass = switches.len() > 10 && wakeups.len() > 5 && matched > 0 && per_cpu_monotonic && tgid > 0;
    if pass {
        println!("\n[PASS] Events captured, TIDs match, per-CPU timestamps monotonic, tgid={tgid}");
    } else {
        println!("\n[FAIL] Insufficient data or validation failure");
        std::process::exit(1);
    }

    println!("\n============================================================");
    Ok(())
}

fn spawn_mutex_workload() -> (u32, u32) {
    use std::sync::Mutex;

    let mutex = Arc::new(Mutex::new(0u64));
    let tid_a = Arc::new(AtomicU32::new(0));
    let tid_b = Arc::new(AtomicU32::new(0));

    let m = mutex.clone();
    let t = tid_a.clone();
    std::thread::spawn(move || {
        t.store(nix::unistd::gettid().as_raw() as u32, Ordering::SeqCst);
        for _ in 0..200 {
            let _g = m.lock().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    });

    let m = mutex.clone();
    let t = tid_b.clone();
    std::thread::spawn(move || {
        t.store(nix::unistd::gettid().as_raw() as u32, Ordering::SeqCst);
        for _ in 0..200 {
            let _g = m.lock().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    });

    std::thread::sleep(Duration::from_millis(50));
    (tid_a.load(Ordering::SeqCst), tid_b.load(Ordering::SeqCst))
}
