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
    /// BPF skeleton / transport pipeline not yet wired.
    NotYetWired,
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
            Self::NotYetWired => write!(
                f,
                "record pipeline not yet wired: BPF skeleton build pipeline is required \
                 but not yet integrated"
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
            Self::NoBpfSupport | Self::NotYetWired => None,
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
    // `signal_hook::flag::register` stores `true` into the AtomicBool on signal,
    // so we start at `false` and poll `stop_requested.load()` to detect shutdown.
    let stop_requested = Arc::new(AtomicBool::new(false));
    register_signal_handlers(&stop_requested)?;

    // Step 2-6: BPF pipeline (feature-gated).
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

/// BPF-enabled record implementation.
///
/// Currently returns `NotYetWired` because the BPF skeleton build pipeline
/// is not yet integrated. Once skeleton wiring lands, this function will:
/// 1. `probe_all()` → `FeatureMatrix`
/// 2. Open skeleton → configure transport (ringbuf/perfarray) → load → attach
/// 3. Poll transport in loop while `!stop_requested`
/// 4. Drain → `writer.finish(drop_count)`
#[cfg(feature = "bpf")]
fn record_impl(_args: &RecordArgs, _stop_requested: &Arc<AtomicBool>) -> Result<(), RecordError> {
    // The BPF skeleton build pipeline is not yet wired.
    // Returning an explicit error avoids producing a misleading empty trace.
    Err(RecordError::NotYetWired)
}

/// Non-BPF build: runtime error at invocation boundary.
#[cfg(not(feature = "bpf"))]
fn record_impl(_args: &RecordArgs, _stop_requested: &Arc<AtomicBool>) -> Result<(), RecordError> {
    Err(RecordError::NoBpfSupport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn signal_handler_registration_succeeds() {
        let stop_requested = Arc::new(AtomicBool::new(false));
        register_signal_handlers(&stop_requested).unwrap();
        // Flag should still be false (no signal sent).
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
    fn record_error_display_not_yet_wired() {
        let err = RecordError::NotYetWired;
        let msg = format!("{err}");
        assert!(msg.contains("not yet wired"));
        assert!(msg.contains("skeleton"));
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

        let err = RecordError::NotYetWired;
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
        let stop_requested = Arc::new(AtomicBool::new(false));
        let result = record_impl(&args, &stop_requested);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RecordError::NoBpfSupport));
    }

    #[cfg(feature = "bpf")]
    #[test]
    fn record_bpf_returns_not_yet_wired() {
        let args = RecordArgs {
            output: PathBuf::from("/tmp/test.wperf"),
            duration: None,
            buffer_size: None,
        };
        let stop_requested = Arc::new(AtomicBool::new(false));
        let result = record_impl(&args, &stop_requested);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RecordError::NotYetWired));
    }
}
