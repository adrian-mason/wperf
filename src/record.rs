//! `wperf record` subcommand — collect scheduling events into a .wperf file.
//!
//! Authoritative Inputs:
//! - final-design.md §1.2 (CLI model)
//! - final-design.md §4.1-4.4 (wPRF format + crash recovery)
//! - ADR-002 (feature probing)
//! - ADR-004 (transport abstraction)
//!
//! Current status: CLI scaffold + signal handling + orchestration seam.
//! The actual BPF load → transport → write pipeline is behind
//! `#[cfg(feature = "bpf")]` and requires the skeleton build pipeline
//! to be wired before it can function end-to-end.

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
#[cfg(any(feature = "bpf", test))]
use std::sync::atomic::Ordering;

use crate::cli::RecordArgs;

/// Errors from the record subcommand.
#[derive(Debug)]
pub enum RecordError {
    /// BPF feature not compiled in.
    NoBpfSupport,
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
            Self::SignalSetup(e) => write!(f, "failed to register signal handler: {e}"),
            Self::Io(e) => write!(f, "record I/O error: {e}"),
        }
    }
}

impl std::error::Error for RecordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SignalSetup(e) | Self::Io(e) => Some(e),
            Self::NoBpfSupport => None,
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
/// Orchestration flow (once BPF skeleton is wired):
/// 1. Register SIGINT/SIGTERM handlers → `running` flag
/// 2. `probe::probe_all()` → `FeatureMatrix`
/// 3. Open BPF skeleton → configure transport → load → attach
/// 4. Create `WperfWriter` for output file
/// 5. Poll transport in loop while `running` is true
/// 6. Drain remaining events → `writer.finish(drop_count)`
/// 7. Print summary
pub fn run(args: &RecordArgs) -> Result<(), RecordError> {
    // Step 1: Register signal handlers for graceful shutdown.
    let running = Arc::new(AtomicBool::new(true));
    register_signal_handlers(&running)?;

    // Step 2-6: BPF pipeline (feature-gated).
    record_impl(args, &running)
}

/// Register SIGINT and SIGTERM handlers that clear the `running` flag.
fn register_signal_handlers(running: &Arc<AtomicBool>) -> Result<(), RecordError> {
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, Arc::clone(running)).map_err(RecordError::SignalSetup)?;
    }
    Ok(())
}

/// BPF-enabled record implementation.
#[cfg(feature = "bpf")]
fn record_impl(args: &RecordArgs, running: &Arc<AtomicBool>) -> Result<(), RecordError> {
    use std::fs::File;
    use std::io::BufWriter;
    use std::time::Instant;

    use crate::format::writer::WperfWriter;

    eprintln!("wperf: recording to {} ...", args.output.display());
    if let Some(d) = args.duration {
        eprintln!("wperf: duration limit: {d:.1}s");
    }

    // Create output file and writer.
    let file = File::create(&args.output)?;
    let mut writer = WperfWriter::new(BufWriter::new(file))?;

    // TODO: probe_all() → FeatureMatrix → skeleton open → configure → load → attach
    // This is blocked on the BPF skeleton build pipeline (#5 closeout).
    //
    // For now, the record loop structure is in place but there is no
    // transport to poll. We write an empty trace with a valid footer.

    let start = Instant::now();
    let deadline = args
        .duration
        .map(|d| start + std::time::Duration::from_secs_f64(d));

    // Event collection loop (placeholder — no transport yet).
    while running.load(Ordering::Relaxed) {
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                break;
            }
        }
        // Once transport is wired:
        //   transport.poll(100, &mut |event| { writer.write_event(event).unwrap(); });
        //
        // For now, just sleep briefly to avoid busy-looping.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Drain + finalize.
    // Once transport is wired: transport.drain(...)
    let drop_count = 0_u64; // Will come from transport.drop_count()
    let event_count = writer.event_count();
    writer.finish(drop_count)?;

    let elapsed = start.elapsed();
    eprintln!(
        "wperf: recorded {event_count} events in {:.1}s ({drop_count} drops) → {}",
        elapsed.as_secs_f64(),
        args.output.display(),
    );

    Ok(())
}

/// Non-BPF build: runtime error at invocation boundary.
#[cfg(not(feature = "bpf"))]
fn record_impl(_args: &RecordArgs, _running: &Arc<AtomicBool>) -> Result<(), RecordError> {
    Err(RecordError::NoBpfSupport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn signal_handler_registration_succeeds() {
        let running = Arc::new(AtomicBool::new(true));
        // Should not panic or error on a normal system.
        register_signal_handlers(&running).unwrap();
        assert!(running.load(Ordering::Relaxed));
    }

    #[test]
    fn record_error_display_no_bpf() {
        let err = RecordError::NoBpfSupport;
        let msg = format!("{err}");
        assert!(msg.contains("without BPF support"));
        assert!(msg.contains("--features bpf"));
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

        let err = RecordError::SignalSetup(io::Error::other("x"));
        assert!(std::error::Error::source(&err).is_some());

        let err = RecordError::Io(io::Error::other("y"));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[cfg(not(feature = "bpf"))]
    #[test]
    fn record_without_bpf_returns_error() {
        let args = RecordArgs {
            output: PathBuf::from("/tmp/test.wperf"),
            duration: None,
            buffer_size: None,
        };
        let running = Arc::new(AtomicBool::new(true));
        let result = record_impl(&args, &running);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, RecordError::NoBpfSupport));
    }

    #[cfg(feature = "bpf")]
    #[test]
    fn record_with_duration_produces_valid_file() {
        let dir = std::env::temp_dir().join("wperf-test-record");
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("test-record.wperf");

        let args = RecordArgs {
            output: output.clone(),
            duration: Some(0.2), // 200ms — just enough to exercise the loop
            buffer_size: None,
        };
        let running = Arc::new(AtomicBool::new(true));
        record_impl(&args, &running).unwrap();

        // Verify the file is a valid .wperf with header.
        let data = std::fs::read(&output).unwrap();
        assert!(data.len() >= 64); // At least header
        assert_eq!(&data[0..4], b"wPRF");

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
