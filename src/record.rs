//! `wperf record` subcommand — collect scheduling events into a .wperf file.
//!
//! Authoritative Inputs:
//! - final-design.md §1.2 (CLI model)
//! - final-design.md §4.1-4.4 (wPRF format + crash recovery)
//! - ADR-002 (feature probing)
//! - ADR-004 (transport abstraction)
//! - ADR-013 (dual-variant `sched_switch` + `sched_wakeup` probes)

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;

use crate::cli::RecordArgs;

#[cfg(feature = "bpf")]
#[allow(unused_imports, clippy::all, clippy::pedantic)]
mod skel {
    include!(concat!(env!("OUT_DIR"), "/wperf.skel.rs"));
}

/// Errors from the record subcommand.
#[derive(Debug)]
pub enum RecordError {
    /// BPF feature not compiled in.
    NoBpfSupport,
    /// Feature probing failed (e.g., no BTF).
    Probe(String),
    /// BPF skeleton operation failed (open/load/attach).
    Bpf(String),
    /// Signal handler registration failed.
    SignalSetup(io::Error),
    /// I/O error (file creation, write, etc).
    Io(io::Error),
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBpfSupport => write!(
                f,
                "this build of wperf was compiled without BPF support; \
                 rebuild with `--features bpf`"
            ),
            Self::Probe(msg) => write!(f, "feature probing failed: {msg}"),
            Self::Bpf(msg) => write!(f, "BPF error: {msg}"),
            Self::SignalSetup(e) => write!(f, "failed to register signal handler: {e}"),
            Self::Io(e) => write!(f, "record I/O error: {e}"),
        }
    }
}

impl std::error::Error for RecordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SignalSetup(e) | Self::Io(e) => Some(e),
            Self::NoBpfSupport | Self::Probe(_) | Self::Bpf(_) => None,
        }
    }
}

impl From<io::Error> for RecordError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Run the `wperf record` subcommand.
///
/// Orchestration flow:
/// 1. Register SIGINT/SIGTERM handlers → `stop_requested` flag
/// 2. `probe::probe_all()` → `FeatureMatrix`
/// 3. Open BPF skeleton → disable unused variant → load → attach
/// 4. Create `WperfWriter` for output file
/// 5. Poll transport in loop while `!stop_requested` (+ optional duration)
/// 6. Drain remaining events → `writer.finish(drop_count)`
/// 7. Print summary
pub fn run(args: &RecordArgs) -> Result<(), RecordError> {
    let stop_requested = Arc::new(AtomicBool::new(false));
    register_signal_handlers(&stop_requested)?;
    record_impl(args, &stop_requested)
}

/// Register SIGINT and SIGTERM handlers that set `stop_requested` to true.
fn register_signal_handlers(stop_requested: &Arc<AtomicBool>) -> Result<(), RecordError> {
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, Arc::clone(stop_requested))
            .map_err(RecordError::SignalSetup)?;
    }
    Ok(())
}

/// Return the global (init-namespace) TGID for the current process.
///
/// BPF probes see the init-namespace TGID, which differs from
/// `std::process::id()` inside PID namespaces (containers). Reads
/// `/proc/self/status` `NSpid` field (first value = outermost TGID).
/// Falls back to `std::process::id()` if `NSpid` is unavailable.
#[cfg(feature = "bpf")]
fn global_tgid() -> u32 {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(first) = line
                .strip_prefix("NSpid:")
                .and_then(|rest| rest.split_whitespace().next())
                && let Ok(pid) = first.parse::<u32>()
            {
                return pid;
            }
        }
    }
    std::process::id()
}

#[cfg(feature = "bpf")]
#[allow(clippy::too_many_lines)]
fn record_impl(args: &RecordArgs, stop_requested: &Arc<AtomicBool>) -> Result<(), RecordError> {
    use std::fs::File;
    use std::io::BufWriter;
    use std::time::Instant;

    use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};

    use crate::format::writer::WperfWriter;
    use crate::probe::{self, ProbePaths, TracepointMode, TransportMode};
    use crate::transport::TransportConfig;

    // --- Step 1: Feature probing ---
    let paths = ProbePaths::default();
    let features = probe::probe_all(&paths).map_err(|e| RecordError::Probe(e.to_string()))?;
    let transport_config = match args.buffer_size {
        Some(size) => match features.transport {
            TransportMode::RingBuf => TransportConfig::ringbuf(size),
            TransportMode::PerfArray => {
                let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
                if raw <= 0 {
                    return Err(RecordError::Io(std::io::Error::other(
                        "failed to retrieve system page size via sysconf",
                    )));
                }
                let page_size: u32 = raw.try_into().expect("page size must fit in u32");
                let pages = size.div_ceil(page_size).next_power_of_two();
                TransportConfig::perfarray(pages)
            }
        },
        None => TransportConfig::from_mode(features.transport),
    };

    eprintln!(
        "wperf: transport={:?}, tracepoint={:?}",
        features.transport, features.tracepoint,
    );

    // --- Step 2: Open → configure → load → attach ---
    let mut open_obj = std::mem::MaybeUninit::uninit();
    let mut open_skel = skel::WperfSkelBuilder::default()
        .open(&mut open_obj)
        .map_err(|e| RecordError::Bpf(format!("skeleton open: {e}")))?;

    match features.tracepoint {
        TracepointMode::TpBtf => {
            open_skel.progs.handle_sched_switch_raw.set_autoload(false);
            open_skel.progs.handle_sched_wakeup_raw.set_autoload(false);
            open_skel
                .progs
                .handle_block_rq_issue_raw
                .set_autoload(false);
            open_skel
                .progs
                .handle_block_rq_complete_raw
                .set_autoload(false);
        }
        TracepointMode::RawTp => {
            open_skel.progs.handle_sched_switch_btf.set_autoload(false);
            open_skel.progs.handle_sched_wakeup_btf.set_autoload(false);
            open_skel
                .progs
                .handle_block_rq_issue_btf
                .set_autoload(false);
            open_skel
                .progs
                .handle_block_rq_complete_btf
                .set_autoload(false);
        }
    }

    open_skel
        .maps
        .bss_data
        .as_mut()
        .ok_or_else(|| RecordError::Bpf("BSS data not available".into()))?
        .self_tgid = global_tgid();

    {
        let rodata = open_skel
            .maps
            .rodata_data
            .as_mut()
            .ok_or_else(|| RecordError::Bpf("rodata not available".into()))?;
        rodata.enable_futex_tracing = true;
        rodata.targ_single = features.block_rq_issue_single_arg;
    }

    if features.transport == TransportMode::RingBuf {
        open_skel
            .maps
            .events
            .set_max_entries(transport_config.ringbuf_size)
            .map_err(|e| RecordError::Bpf(format!("set ringbuf size: {e}")))?;
    }

    let mut loaded_skel = open_skel
        .load()
        .map_err(|e| RecordError::Bpf(format!("skeleton load: {e}")))?;

    loaded_skel
        .attach()
        .map_err(|e| RecordError::Bpf(format!("skeleton attach: {e}")))?;

    eprintln!(
        "wperf: probes attached, recording to {}",
        args.output.display()
    );

    // --- Step 3: Create writer ---
    let file = File::create(&args.output)?;
    let buf_writer = BufWriter::with_capacity(1024 * 1024, file);
    let mut writer = WperfWriter::new(buf_writer)?;

    // --- Step 4: Poll loop ---
    let start = Instant::now();
    let deadline = args
        .duration
        .map(|d| start + std::time::Duration::from_secs_f64(d));
    let mut event_count: u64 = 0;
    let mut transport_lost: u64 = 0;

    match features.transport {
        TransportMode::RingBuf => {
            poll_ringbuf(
                &loaded_skel,
                &mut writer,
                &mut event_count,
                stop_requested,
                deadline,
            )?;
        }
        TransportMode::PerfArray => {
            transport_lost = poll_perfarray(
                &loaded_skel,
                &mut writer,
                &mut event_count,
                stop_requested,
                deadline,
                &transport_config,
            )?;
        }
    }

    // --- Step 5: Finish ---
    // BPF-side drops (ringbuf reserve failures) + transport-side drops (perfarray lost events).
    let bpf_drops = loaded_skel
        .maps
        .bss_data
        .as_ref()
        .map_or(0, |bss| bss.drop_counter);
    let drop_count = bpf_drops + transport_lost;

    let inner = writer.finish(drop_count)?;
    inner
        .into_inner()
        .map_err(|e| RecordError::Io(e.into_error()))?;

    let elapsed = start.elapsed();
    eprintln!(
        "wperf: recording complete — {event_count} events, {drop_count} drops, {:.1}s",
        elapsed.as_secs_f64(),
    );

    Ok(())
}

/// Ringbuf poll loop: events arrive in global timestamp order.
#[cfg(feature = "bpf")]
fn poll_ringbuf<W: std::io::Write + std::io::Seek>(
    skel: &skel::WperfSkel<'_>,
    writer: &mut crate::format::writer::WperfWriter<W>,
    event_count: &mut u64,
    stop_requested: &Arc<AtomicBool>,
    deadline: Option<std::time::Instant>,
) -> Result<(), RecordError> {
    use std::cell::RefCell;
    use std::sync::atomic::Ordering;

    use crate::format::event::EVENT_SIZE;

    let writer = RefCell::new(writer);
    let count = RefCell::new(event_count);
    let write_err: RefCell<Option<io::Error>> = RefCell::new(None);

    let mut builder = libbpf_rs::RingBufferBuilder::new();
    builder
        .add(&skel.maps.events, |data: &[u8]| {
            if data.len() < EVENT_SIZE {
                return 0;
            }
            let raw: &[u8; EVENT_SIZE] = data[..EVENT_SIZE].try_into().unwrap();
            let mut w = writer.borrow_mut();
            if let Err(e) = w.write_event_raw(raw) {
                *write_err.borrow_mut() = Some(e);
                return -1;
            }
            **count.borrow_mut() += 1;
            0
        })
        .map_err(|e| RecordError::Bpf(format!("ringbuf builder: {e}")))?;
    let ringbuf = builder
        .build()
        .map_err(|e| RecordError::Bpf(format!("ringbuf build: {e}")))?;

    loop {
        if stop_requested.load(Ordering::Relaxed) {
            break;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        let prev_count = **count.borrow();
        let _ = ringbuf.consume();
        if let Some(e) = write_err.borrow_mut().take() {
            return Err(RecordError::Io(e));
        }
        if **count.borrow() == prev_count {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // Final drain.
    let _ = ringbuf.consume();
    if let Some(e) = write_err.borrow_mut().take() {
        return Err(RecordError::Io(e));
    }

    Ok(())
}

/// Perfarray poll loop: events arrive per-CPU, reordered via `ReorderBuf`.
///
/// `PerfBuffer` callbacks collect events into a shared Vec, which the main loop
/// drains through the reorder buffer into the writer.
#[cfg(feature = "bpf")]
fn poll_perfarray<W: std::io::Write + std::io::Seek>(
    skel: &skel::WperfSkel<'_>,
    writer: &mut crate::format::writer::WperfWriter<W>,
    event_count: &mut u64,
    stop_requested: &Arc<AtomicBool>,
    deadline: Option<std::time::Instant>,
    config: &crate::transport::TransportConfig,
) -> Result<u64, RecordError> {
    use std::cell::RefCell;
    use std::sync::atomic::Ordering;

    use crate::format::event::{EVENT_SIZE, WperfEvent};
    use crate::transport::ReorderBuf;

    let pending: RefCell<Vec<WperfEvent>> = RefCell::new(Vec::with_capacity(4096));
    let lost_count: RefCell<u64> = RefCell::new(0);

    let perf = libbpf_rs::PerfBufferBuilder::new(&skel.maps.events)
        .pages(config.perf_pages as usize)
        .sample_cb(|_cpu: i32, data: &[u8]| {
            if data.len() < EVENT_SIZE {
                return;
            }
            let buf: &[u8; EVENT_SIZE] = data[..EVENT_SIZE].try_into().unwrap();
            let event = WperfEvent::from_bytes(buf);
            pending.borrow_mut().push(event);
        })
        .lost_cb(|_cpu: i32, count: u64| {
            *lost_count.borrow_mut() += count;
        })
        .build()
        .map_err(|e| RecordError::Bpf(format!("perf buffer build: {e}")))?;

    let mut reorder = ReorderBuf::new();
    let write_err: RefCell<Option<io::Error>> = RefCell::new(None);

    let mut write_cb = |ev: &WperfEvent| {
        if write_err.borrow().is_some() {
            return;
        }
        match writer.write_event(ev) {
            Ok(()) => *event_count += 1,
            Err(e) => *write_err.borrow_mut() = Some(e),
        }
    };

    loop {
        if stop_requested.load(Ordering::Relaxed) {
            break;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        let _ = perf.poll(std::time::Duration::from_millis(500));

        for event in pending.borrow_mut().drain(..) {
            reorder.push(event, &mut write_cb);
        }
        if let Some(e) = write_err.borrow_mut().take() {
            return Err(RecordError::Io(e));
        }
    }

    // Final drain: flush pending events from last poll, then reorder buffer.
    for event in pending.borrow_mut().drain(..) {
        reorder.push(event, &mut write_cb);
    }
    if let Some(e) = write_err.borrow_mut().take() {
        return Err(RecordError::Io(e));
    }
    reorder.drain(&mut write_cb);
    if let Some(e) = write_err.borrow_mut().take() {
        return Err(RecordError::Io(e));
    }

    Ok(*lost_count.borrow())
}

/// Non-BPF build: runtime error at invocation boundary.
#[cfg(not(feature = "bpf"))]
fn record_impl(_args: &RecordArgs, _stop_requested: &Arc<AtomicBool>) -> Result<(), RecordError> {
    Err(RecordError::NoBpfSupport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_handler_registration_succeeds() {
        let stop_requested = Arc::new(AtomicBool::new(false));
        register_signal_handlers(&stop_requested).unwrap();
        assert!(!stop_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn record_error_display_no_bpf() {
        let err = RecordError::NoBpfSupport;
        let msg = format!("{err}");
        assert!(msg.contains("without BPF support"));
        assert!(msg.contains("--features bpf"));
    }

    #[test]
    fn record_error_display_probe() {
        let err = RecordError::Probe("BTF not available".to_string());
        let msg = format!("{err}");
        assert!(msg.contains("feature probing failed"));
        assert!(msg.contains("BTF"));
    }

    #[test]
    fn record_error_display_bpf() {
        let err = RecordError::Bpf("load failed".to_string());
        let msg = format!("{err}");
        assert!(msg.contains("BPF error"));
    }

    #[test]
    fn record_error_display_signal() {
        let err = RecordError::SignalSetup(io::Error::other("test"));
        let msg = format!("{err}");
        assert!(msg.contains("signal handler"));
        assert!(msg.contains("test"));
    }

    #[test]
    fn record_error_display_io() {
        let err = RecordError::Io(io::Error::other("disk full"));
        let msg = format!("{err}");
        assert!(msg.contains("I/O error"));
        assert!(msg.contains("disk full"));
    }

    #[test]
    fn record_error_source() {
        let err = RecordError::NoBpfSupport;
        assert!(std::error::Error::source(&err).is_none());

        let err = RecordError::Probe("x".to_string());
        assert!(std::error::Error::source(&err).is_none());

        let err = RecordError::Bpf("x".to_string());
        assert!(std::error::Error::source(&err).is_none());

        let err = RecordError::SignalSetup(io::Error::other("x"));
        assert!(std::error::Error::source(&err).is_some());

        let err = RecordError::Io(io::Error::other("y"));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[cfg(not(feature = "bpf"))]
    #[test]
    fn record_without_bpf_returns_error() {
        let args = RecordArgs {
            output: std::path::PathBuf::from("/tmp/test.wperf"),
            duration: None,
            buffer_size: None,
        };
        let stop_requested = Arc::new(AtomicBool::new(false));
        let result = record_impl(&args, &stop_requested);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RecordError::NoBpfSupport));
    }
}
