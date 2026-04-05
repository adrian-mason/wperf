//! Transport configuration derived from feature probing results.

use crate::probe::TransportMode;

/// Default ring buffer size: 16 MiB.
const DEFAULT_RINGBUF_SIZE: u32 = 16 * 1024 * 1024;

/// Default perfarray page count per CPU (256 pages = 1 MiB per CPU at 4K pages).
const DEFAULT_PERF_PAGES: u32 = 256;

/// Configuration for the event transport layer.
///
/// Created from [`FeatureMatrix`](crate::probe::FeatureMatrix) probe results
/// and optional CLI overrides (`--buffer-size`).
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Transport mode determined by startup probing.
    pub mode: TransportMode,
    /// Ring buffer size in bytes (must be power of 2, multiple of page size).
    /// Only used when `mode == RingBuf`.
    pub ringbuf_size: u32,
    /// Per-CPU page count for perfarray buffers.
    /// Only used when `mode == PerfArray`.
    pub perf_pages: u32,
}

impl TransportConfig {
    /// Create a config for ring buffer transport with the given buffer size.
    ///
    /// `size` must be a power of 2 and at least one page (4096 bytes).
    /// Panics in debug mode if `size` is not a power of 2.
    pub fn ringbuf(size: u32) -> Self {
        debug_assert!(size.is_power_of_two(), "ringbuf size must be power of 2");
        debug_assert!(size >= 4096, "ringbuf size must be at least one page");
        Self {
            mode: TransportMode::RingBuf,
            ringbuf_size: size,
            perf_pages: 0,
        }
    }

    /// Create a config for perfarray transport with the given per-CPU page count.
    pub fn perfarray(pages_per_cpu: u32) -> Self {
        Self {
            mode: TransportMode::PerfArray,
            ringbuf_size: 0,
            perf_pages: pages_per_cpu,
        }
    }

    /// Create a config from the probed transport mode using default buffer sizes.
    pub fn from_mode(mode: TransportMode) -> Self {
        match mode {
            TransportMode::RingBuf => Self::ringbuf(DEFAULT_RINGBUF_SIZE),
            TransportMode::PerfArray => Self::perfarray(DEFAULT_PERF_PAGES),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ringbuf_config_defaults() {
        let cfg = TransportConfig::from_mode(TransportMode::RingBuf);
        assert_eq!(cfg.mode, TransportMode::RingBuf);
        assert_eq!(cfg.ringbuf_size, 16 * 1024 * 1024);
        assert_eq!(cfg.perf_pages, 0);
    }

    #[test]
    fn perfarray_config_defaults() {
        let cfg = TransportConfig::from_mode(TransportMode::PerfArray);
        assert_eq!(cfg.mode, TransportMode::PerfArray);
        assert_eq!(cfg.perf_pages, 256);
        assert_eq!(cfg.ringbuf_size, 0);
    }

    #[test]
    fn ringbuf_custom_size() {
        let cfg = TransportConfig::ringbuf(8 * 1024 * 1024);
        assert_eq!(cfg.ringbuf_size, 8 * 1024 * 1024);
    }

    #[test]
    fn perfarray_custom_pages() {
        let cfg = TransportConfig::perfarray(512);
        assert_eq!(cfg.perf_pages, 512);
    }

    #[test]
    fn from_mode_ringbuf_is_power_of_two() {
        let cfg = TransportConfig::from_mode(TransportMode::RingBuf);
        assert!(cfg.ringbuf_size.is_power_of_two());
    }
}
