//! Dynamic feature probing for kernel eBPF capabilities.
//!
//! Implements the ADR-002 per-feature probing matrix. Each feature is tested
//! independently at startup between skeleton `open()` and `load()`.
//!
//! Probes are organized into three tiers:
//! - **Tier 1** (filesystem checks): BTF, cgroupv2, tracepoint existence, kprobe existence
//! - **Tier 2** (syscall-level): ringbuf map creation, `bpf_loop` helper availability
//! - **Tier 3** (attach-based): `tp_btf` attach test, `fentry` attach test

use std::io;
use std::path::Path;

/// Errors that can occur during feature probing.
#[derive(Debug)]
pub enum ProbeError {
    /// BTF is required but not available on this kernel.
    BtfRequired,
    /// An I/O error occurred during probing.
    Io(io::Error),
    /// A syscall-level probe failed in an unexpected way.
    Syscall(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BtfRequired => write!(
                f,
                "BTF not available: /sys/kernel/btf/vmlinux not found. \
                 Minimum supported: RHEL 8.2+ / Rocky 8.4+ with CONFIG_DEBUG_INFO_BTF=y"
            ),
            Self::Io(e) => write!(f, "I/O error during feature probing: {e}"),
            Self::Syscall(msg) => write!(f, "syscall probe failed: {msg}"),
        }
    }
}

impl std::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::BtfRequired | Self::Syscall(_) => None,
        }
    }
}

impl From<io::Error> for ProbeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Transport mode selected by the ringbuf probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    /// Ring buffer available (kernel 5.8+). Zero-copy path via `bpf_ringbuf_reserve`/`submit`.
    RingBuf,
    /// Perfarray fallback. Requires percpu-array staging + Min-Heap Reorder Buffer in userspace.
    PerfArray,
}

/// Scheduler tracepoint attachment mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracepointMode {
    /// `tp_btf/sched_switch` + `tp_btf/sched_wakeup` (kernel 5.5+).
    /// Direct `task_struct *` access without casts.
    TpBtf,
    /// `raw_tp/sched_switch` + `raw_tp/sched_wakeup` (kernel 4.17+).
    /// Requires `bpf_probe_read_kernel` for field access.
    RawTp,
}

/// Result of probing all kernel eBPF features at startup.
///
/// Consumed by the skeleton reconfiguration step between `open()` and `load()`.
/// See ADR-002 §Decision for the full probe matrix.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct FeatureMatrix {
    /// Transport mode: `RingBuf` (preferred) or `PerfArray` (fallback).
    pub transport: TransportMode,
    /// Scheduler tracepoint mode: `TpBtf` (preferred) or `RawTp` (fallback).
    pub tracepoint: TracepointMode,
    /// Whether `/sys/kernel/btf/vmlinux` is present. Hard requirement for Phase 1.
    pub has_btf: bool,
    /// Whether `bpf_loop` helper is available (kernel 5.17+).
    pub has_bpf_loop: bool,
    /// Whether cgroupv2 is mounted and functional.
    pub has_cgroupv2: bool,
    /// Whether `fentry` attachment is supported (kernel 5.5+).
    pub has_fentry: bool,
}

/// Sysfs/tracefs paths used by the probing layer.
///
/// Extracted as a struct so tests can override paths without touching the real filesystem.
pub struct ProbePaths<'a> {
    pub btf_vmlinux: &'a Path,
    pub cgroup_controllers: &'a Path,
    pub tracing_events: &'a Path,
    pub kprobe_blacklist: &'a Path,
    pub available_filter_functions: &'a Path,
}

impl Default for ProbePaths<'_> {
    fn default() -> Self {
        Self {
            btf_vmlinux: Path::new("/sys/kernel/btf/vmlinux"),
            cgroup_controllers: Path::new("/sys/fs/cgroup/cgroup.controllers"),
            tracing_events: Path::new("/sys/kernel/tracing/events"),
            kprobe_blacklist: Path::new("/sys/kernel/debug/kprobes/blacklist"),
            available_filter_functions: Path::new(
                "/sys/kernel/debug/tracing/available_filter_functions",
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier 1: Filesystem-based probes
// ---------------------------------------------------------------------------

/// Check whether BTF is available via `/sys/kernel/btf/vmlinux`.
///
/// This is a hard requirement for Phase 1. Returns `Err(ProbeError::BtfRequired)`
/// if BTF is not found — the caller should abort startup.
pub fn probe_btf(paths: &ProbePaths<'_>) -> Result<bool, ProbeError> {
    if paths.btf_vmlinux.exists() {
        Ok(true)
    } else {
        Err(ProbeError::BtfRequired)
    }
}

/// Check whether cgroupv2 is mounted by testing `/sys/fs/cgroup/cgroup.controllers`.
pub fn probe_cgroupv2(paths: &ProbePaths<'_>) -> bool {
    paths.cgroup_controllers.exists()
}

/// Check whether a specific tracepoint exists in tracefs.
///
/// Probes `/sys/kernel/tracing/events/{category}/{name}`.
pub fn probe_tracepoint(paths: &ProbePaths<'_>, category: &str, name: &str) -> bool {
    paths.tracing_events.join(category).join(name).exists()
}

/// Check whether a kernel function is available for kprobe attachment.
///
/// A function is kprobe-eligible if it appears in `available_filter_functions`
/// and does NOT appear in the kprobe blacklist.
pub fn probe_kprobe(paths: &ProbePaths<'_>, function: &str) -> Result<bool, ProbeError> {
    let in_blacklist = file_contains_line(paths.kprobe_blacklist, function)?;
    if in_blacklist {
        return Ok(false);
    }
    file_contains_line(paths.available_filter_functions, function)
}

/// Scan a file line-by-line for a line containing `needle`.
///
/// Returns `Ok(false)` if the file does not exist (non-fatal for optional debugfs files).
fn file_contains_line(path: &Path, needle: &str) -> Result<bool, ProbeError> {
    use std::io::BufRead;

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(ProbeError::Io(e)),
    };

    let reader = io::BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        // available_filter_functions format: "func_name [module]"
        // kprobe blacklist format: "0xaddr\tfunc_name"
        // In both cases, check if the function name appears as a word boundary.
        if line.split_whitespace().any(|word| word == needle) {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// Tier 2: Syscall-level probes (require libbpf-rs at runtime)
// ---------------------------------------------------------------------------

/// Probe whether `BPF_MAP_TYPE_RINGBUF` is supported by the running kernel.
///
/// Attempts `bpf_map_create(BPF_MAP_TYPE_RINGBUF, ...)` and closes the fd on success.
/// This follows the `probe_ringbuf()` pattern from libbpf-tools `trace_helpers.c`.
///
/// Requires `CAP_BPF` or `CAP_SYS_ADMIN`.
#[cfg(feature = "bpf")]
#[mutants::skip] // cfg-gated out in default build; mutation has no observable effect on tests
pub fn probe_ringbuf() -> Result<TransportMode, ProbeError> {
    use libbpf_rs::MapType;

    // Attempt to create a minimal ringbuf map. Page-size is the minimum.
    // Mirrors trace_helpers.c probe_ringbuf(): bpf_map_create(RINGBUF, NULL, 0, 0, page_size, NULL)
    let page_size_raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u32::try_from(page_size_raw).unwrap_or(4096);
    let mut opts: libbpf_sys::bpf_map_create_opts = unsafe { std::mem::zeroed() };
    opts.sz = std::mem::size_of::<libbpf_sys::bpf_map_create_opts>() as libbpf_sys::size_t;

    match libbpf_rs::MapHandle::create(MapType::RingBuf, None::<&str>, 0, 0, page_size, &opts) {
        Ok(_map) => Ok(TransportMode::RingBuf), // fd closed on drop
        Err(_) => Ok(TransportMode::PerfArray),
    }
}

/// Stub for environments without `libbpf-rs`. Always returns `PerfArray`.
#[cfg(not(feature = "bpf"))]
#[allow(clippy::unnecessary_wraps)]
pub fn probe_ringbuf() -> Result<TransportMode, ProbeError> {
    Ok(TransportMode::PerfArray)
}

/// Probe whether the `bpf_loop` helper is available (kernel 5.17+).
///
/// Uses `libbpf_probe_bpf_helper` to check `BPF_FUNC_loop` availability
/// for `BPF_PROG_TYPE_TRACEPOINT` programs.
#[cfg(feature = "bpf")]
#[mutants::skip] // cfg-gated out in default build; mutation has no observable effect on tests
pub fn probe_bpf_loop() -> Result<bool, ProbeError> {
    // libbpf_probe_bpf_helper(BPF_PROG_TYPE_TRACEPOINT, BPF_FUNC_loop, NULL)
    // libbpf-rs exposes this via libbpf_sys.
    let ret = unsafe {
        libbpf_sys::libbpf_probe_bpf_helper(
            libbpf_sys::BPF_PROG_TYPE_TRACEPOINT,
            libbpf_sys::BPF_FUNC_loop,
            std::ptr::null(),
        )
    };
    match ret {
        1 => Ok(true),  // supported
        0 => Ok(false), // not supported
        neg => Err(ProbeError::Syscall(format!(
            "libbpf_probe_bpf_helper returned {neg}"
        ))),
    }
}

/// Stub for environments without `libbpf-rs`. Always returns `false`.
#[cfg(not(feature = "bpf"))]
#[allow(clippy::unnecessary_wraps)]
pub fn probe_bpf_loop() -> Result<bool, ProbeError> {
    Ok(false)
}

// ---------------------------------------------------------------------------
// Tier 3: Attach-based probes (require minimal BPF program loading)
// ---------------------------------------------------------------------------

/// Probe whether `tp_btf` attachment is supported for scheduler tracepoints.
///
/// Requires loading a minimal BPF program that attaches to `tp_btf/sched_switch`.
/// Falls back to `RawTp` if the attach fails.
///
/// Full implementation depends on skeleton infrastructure from task #5.
#[cfg(feature = "bpf")]
#[mutants::skip] // cfg-gated out in default build; mutation has no observable effect on tests
pub fn probe_tp_btf() -> Result<TracepointMode, ProbeError> {
    // TODO(probe): Implement minimal tp_btf attach test once skeleton infra lands.
    // For now, attempt detection via BTF type existence as a proxy:
    // if /sys/kernel/btf/vmlinux exists and kernel >= 5.5, tp_btf is likely available.
    // The real test will use an actual attach attempt.
    Ok(TracepointMode::RawTp)
}

/// Stub without `libbpf-rs`. Always returns `RawTp`.
#[cfg(not(feature = "bpf"))]
#[allow(clippy::unnecessary_wraps)]
pub fn probe_tp_btf() -> Result<TracepointMode, ProbeError> {
    Ok(TracepointMode::RawTp)
}

/// Probe whether `fentry` attachment is supported.
///
/// Requires loading a minimal BPF program that attaches via fentry.
/// Full implementation depends on skeleton infrastructure from task #5.
#[cfg(feature = "bpf")]
#[mutants::skip] // cfg-gated out in default build; mutation has no observable effect on tests
pub fn probe_fentry() -> Result<bool, ProbeError> {
    // TODO(probe): Implement minimal fentry attach test once skeleton infra lands.
    Ok(false)
}

/// Stub without `libbpf-rs`. Always returns `false`.
#[cfg(not(feature = "bpf"))]
#[allow(clippy::unnecessary_wraps)]
pub fn probe_fentry() -> Result<bool, ProbeError> {
    Ok(false)
}

// ---------------------------------------------------------------------------
// Composite: Run all probes and build the FeatureMatrix
// ---------------------------------------------------------------------------

/// Run the full feature probing sequence and return a `FeatureMatrix`.
///
/// This is the main entry point, called once at startup between skeleton
/// `open()` and `load()`.
///
/// # Errors
///
/// Returns `ProbeError::BtfRequired` if BTF is not available (hard requirement).
/// Other probe failures degrade gracefully to the fallback value, with a
/// diagnostic printed to stderr. This is intentional: a probe that fails
/// unexpectedly (e.g., `EPERM` on `bpf_map_create`) is treated the same as
/// "feature not available" — the tool still runs, just with reduced capabilities.
pub fn probe_all(paths: &ProbePaths<'_>) -> Result<FeatureMatrix, ProbeError> {
    // BTF is a hard requirement — fail fast.
    probe_btf(paths)?;

    let transport = probe_ringbuf().unwrap_or_else(|e| {
        eprintln!("probe: ringbuf probe failed ({e}), falling back to perfarray");
        TransportMode::PerfArray
    });
    let tracepoint = probe_tp_btf().unwrap_or_else(|e| {
        eprintln!("probe: tp_btf probe failed ({e}), falling back to raw_tp");
        TracepointMode::RawTp
    });
    let has_bpf_loop = probe_bpf_loop().unwrap_or_else(|e| {
        eprintln!("probe: bpf_loop probe failed ({e}), assuming unavailable");
        false
    });
    let has_cgroupv2 = probe_cgroupv2(paths);
    let has_fentry = probe_fentry().unwrap_or_else(|e| {
        eprintln!("probe: fentry probe failed ({e}), assuming unavailable");
        false
    });

    Ok(FeatureMatrix {
        transport,
        tracepoint,
        has_btf: true, // we passed the BTF check above
        has_bpf_loop,
        has_cgroupv2,
        has_fentry,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("wperf_probe_test_{name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn test_paths(dir: &Path) -> ProbePaths<'_> {
        // We can't easily return ProbePaths with references to owned PathBufs,
        // so we use leaked strings for test convenience. Tests are short-lived.
        // Instead, create the files at known subpaths and use a helper.
        ProbePaths {
            btf_vmlinux: dir.join("btf_vmlinux").leak(),
            cgroup_controllers: dir.join("cgroup_controllers").leak(),
            tracing_events: dir.join("events").leak(),
            kprobe_blacklist: dir.join("kprobe_blacklist").leak(),
            available_filter_functions: dir.join("available_filter_functions").leak(),
        }
    }

    // -- Tier 1: BTF --

    #[test]
    fn btf_present() {
        let dir = TempDir::new("btf_present");
        fs::write(dir.path().join("btf_vmlinux"), b"").unwrap();
        let paths = test_paths(dir.path());
        assert!(probe_btf(&paths).is_ok());
    }

    #[test]
    fn btf_missing_returns_error() {
        let dir = TempDir::new("btf_missing");
        let paths = test_paths(dir.path());
        let err = probe_btf(&paths).unwrap_err();
        assert!(matches!(err, ProbeError::BtfRequired));
    }

    // -- Tier 1: cgroupv2 --

    #[test]
    fn cgroupv2_present() {
        let dir = TempDir::new("cgroup_present");
        fs::write(dir.path().join("cgroup_controllers"), b"cpu memory").unwrap();
        let paths = test_paths(dir.path());
        assert!(probe_cgroupv2(&paths));
    }

    #[test]
    fn cgroupv2_missing() {
        let dir = TempDir::new("cgroup_missing");
        let paths = test_paths(dir.path());
        assert!(!probe_cgroupv2(&paths));
    }

    // -- Tier 1: tracepoint --

    #[test]
    fn tracepoint_exists() {
        let dir = TempDir::new("tp_exists");
        let tp_dir = dir.path().join("events").join("sched").join("sched_switch");
        fs::create_dir_all(&tp_dir).unwrap();
        let paths = test_paths(dir.path());
        assert!(probe_tracepoint(&paths, "sched", "sched_switch"));
    }

    #[test]
    fn tracepoint_missing() {
        let dir = TempDir::new("tp_missing");
        fs::create_dir_all(dir.path().join("events")).unwrap();
        let paths = test_paths(dir.path());
        assert!(!probe_tracepoint(&paths, "sched", "sched_switch"));
    }

    // -- Tier 1: kprobe --

    #[test]
    fn kprobe_available() {
        let dir = TempDir::new("kprobe_avail");
        fs::write(
            dir.path().join("available_filter_functions"),
            "do_sys_open\nvfs_read\nvfs_write\n",
        )
        .unwrap();
        let paths = test_paths(dir.path());
        assert!(probe_kprobe(&paths, "vfs_read").unwrap());
    }

    #[test]
    fn kprobe_blacklisted() {
        let dir = TempDir::new("kprobe_blacklist");
        fs::write(dir.path().join("available_filter_functions"), "vfs_read\n").unwrap();
        fs::write(
            dir.path().join("kprobe_blacklist"),
            "0xffffffff81234567\tvfs_read\n",
        )
        .unwrap();
        let paths = test_paths(dir.path());
        assert!(!probe_kprobe(&paths, "vfs_read").unwrap());
    }

    #[test]
    fn kprobe_not_in_available() {
        let dir = TempDir::new("kprobe_notfound");
        fs::write(
            dir.path().join("available_filter_functions"),
            "do_sys_open\nvfs_write\n",
        )
        .unwrap();
        let paths = test_paths(dir.path());
        assert!(!probe_kprobe(&paths, "vfs_read").unwrap());
    }

    #[test]
    fn kprobe_no_files_returns_false() {
        let dir = TempDir::new("kprobe_nofiles");
        let paths = test_paths(dir.path());
        // Both files missing — should return false, not error.
        assert!(!probe_kprobe(&paths, "vfs_read").unwrap());
    }

    // -- Tier 2 stubs (no bpf feature) --

    #[test]
    fn ringbuf_stub_returns_perfarray() {
        let result = probe_ringbuf().unwrap();
        assert_eq!(result, TransportMode::PerfArray);
    }

    #[test]
    fn bpf_loop_stub_returns_false() {
        assert!(!probe_bpf_loop().unwrap());
    }

    // -- Tier 3 stubs --

    #[test]
    fn tp_btf_stub_returns_raw_tp() {
        let result = probe_tp_btf().unwrap();
        assert_eq!(result, TracepointMode::RawTp);
    }

    #[test]
    fn fentry_stub_returns_false() {
        assert!(!probe_fentry().unwrap());
    }

    // -- Composite: probe_all --

    #[test]
    fn probe_all_with_btf_succeeds() {
        let dir = TempDir::new("probe_all_ok");
        fs::write(dir.path().join("btf_vmlinux"), b"").unwrap();
        fs::write(dir.path().join("cgroup_controllers"), b"cpu memory").unwrap();
        fs::create_dir_all(dir.path().join("events")).unwrap();
        let paths = test_paths(dir.path());

        let matrix = probe_all(&paths).unwrap();
        assert!(matrix.has_btf);
        assert!(matrix.has_cgroupv2);
        // Stubs: no bpf feature compiled
        assert_eq!(matrix.transport, TransportMode::PerfArray);
        assert_eq!(matrix.tracepoint, TracepointMode::RawTp);
        assert!(!matrix.has_bpf_loop);
        assert!(!matrix.has_fentry);
    }

    #[test]
    fn probe_all_without_btf_fails() {
        let dir = TempDir::new("probe_all_nobtf");
        let paths = test_paths(dir.path());

        let err = probe_all(&paths).unwrap_err();
        assert!(matches!(err, ProbeError::BtfRequired));
    }

    // -- Error trait --

    #[test]
    fn error_display() {
        let err = ProbeError::BtfRequired;
        let msg = err.to_string();
        assert!(msg.contains("BTF not available"));
        assert!(msg.contains("RHEL 8.2+"));
    }

    #[test]
    fn error_display_io() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "no access");
        let err = ProbeError::Io(io_err);
        let msg = err.to_string();
        assert!(msg.contains("I/O error"));
    }

    #[test]
    fn error_display_syscall() {
        let err = ProbeError::Syscall("test failure".to_string());
        let msg = err.to_string();
        assert!(msg.contains("syscall probe failed"));
    }

    #[test]
    fn error_source_io_returns_some() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "no access");
        let err = ProbeError::Io(io_err);
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn error_source_btf_required_returns_none() {
        let err = ProbeError::BtfRequired;
        assert!(std::error::Error::source(&err).is_none());
    }

    // -- file_contains_line edge cases --

    #[test]
    fn file_contains_line_permission_denied_propagates() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("perm_denied");
        let target = dir.path().join("unreadable");
        fs::write(&target, "some content\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();

        let result = file_contains_line(&target, "anything");
        // Restore permissions before assert so cleanup succeeds.
        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o644));
        // Non-NotFound errors (PermissionDenied) must propagate, not return Ok(false).
        assert!(result.is_err());
    }

    // -- Stub return value discrimination --

    #[test]
    fn ringbuf_stub_not_ringbuf() {
        // Ensure the stub specifically returns PerfArray, not RingBuf.
        let result = probe_ringbuf().unwrap();
        assert_ne!(result, TransportMode::RingBuf);
    }

    #[test]
    fn bpf_loop_stub_specifically_false() {
        // Ensure the stub returns exactly false, not true.
        let result = probe_bpf_loop().unwrap();
        assert!(!result);
    }

    // -- probe_all cgroupv2 false path --

    #[test]
    fn probe_all_without_cgroupv2() {
        let dir = TempDir::new("probe_all_nocgroup");
        fs::write(dir.path().join("btf_vmlinux"), b"").unwrap();
        // No cgroup_controllers file
        let paths = test_paths(dir.path());

        let matrix = probe_all(&paths).unwrap();
        assert!(!matrix.has_cgroupv2);
    }
}
