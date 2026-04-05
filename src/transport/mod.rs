//! Transport abstraction layer for BPF event delivery.
//!
//! Implements ADR-004: unified interface over ring buffer (kernel 5.8+)
//! and perfarray (4.3+) transports. The [`EventTransport`] trait hides
//! transport differences from the collector and analysis pipeline.
//!
//! The reorder buffer ([`ReorderBuf`]) restores global timestamp ordering
//! for the perfarray path, where per-CPU buffers arrive out of order.

mod config;
mod reorder;

pub use config::TransportConfig;
pub use reorder::ReorderBuf;

use crate::format::event::WperfEvent;

/// Errors that can occur during transport operations.
#[derive(Debug)]
pub enum TransportError {
    /// The underlying BPF transport returned an error during polling.
    Poll(String),
    /// An I/O error occurred.
    Io(std::io::Error),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Poll(msg) => write!(f, "transport poll error: {msg}"),
            Self::Io(e) => write!(f, "transport I/O error: {e}"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Poll(_) => None,
        }
    }
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Unified interface for BPF event delivery.
///
/// Both ring buffer and perfarray transports implement this trait,
/// allowing the collector to poll for events without knowing the
/// underlying transport mechanism.
///
/// # Callback-based API
///
/// Events are delivered via callback (`FnMut(&WperfEvent)`) to avoid
/// heap allocation on the hot path. The callback is invoked for each
/// event received during a single `poll()` call.
pub trait EventTransport {
    /// Poll for events with the given timeout.
    ///
    /// Invokes `callback` for each event received. Returns the number
    /// of events delivered to the callback.
    ///
    /// For ring buffer transport, events arrive in global timestamp order.
    /// For perfarray transport, events arrive in per-CPU order and must
    /// be reordered by the caller (see [`ReorderBuf`]).
    fn poll(
        &mut self,
        timeout_ms: i32,
        callback: &mut dyn FnMut(&WperfEvent),
    ) -> Result<usize, TransportError>;

    /// Drain any buffered events (perfarray reorder buffer flush).
    ///
    /// For ring buffer transport, this is a no-op (events are globally ordered).
    /// For perfarray transport, this flushes the reorder buffer, delivering
    /// remaining events that may be held for timestamp ordering.
    fn drain(&mut self, callback: &mut dyn FnMut(&WperfEvent)) -> usize;

    /// Return the cumulative count of dropped events.
    ///
    /// - Ring buffer: reads `drop_counter` from BPF BSS section.
    /// - Perfarray: accumulated count from `lost_cb` callbacks.
    fn drop_count(&self) -> u64;
}
