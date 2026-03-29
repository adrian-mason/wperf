# Gate 0 — Signoff

- **Date:** 2026-03-23
- **Kernel:** 6.18.19-1-lts (x86_64, BTF available, 32 CPUs)
- **Toolchain:** clang 22.1, Rust 1.94.0 stable, libbpf-rs 0.26.1, Python 3.12

## Per-Issue Results

| Issue | Title | Result | Evidence |
|-------|-------|--------|----------|
| **#54** | wPerf-origin forensic analysis | **PASS** | 12-section analysis doc; critical discovery: no cascade reference implementation |
| **#8** | Python Cascade understanding | **PASS** | Figure 4: Parser=20ms, Network=80ms, total=100ms (exact match); SCC tests pass |
| **#9** | wPRF v1 roundtrip | **PASS** | 10-event roundtrip match; truncation recovery of 6/10 events; header = 64B exact |
| **#7** | eBPF minimal collection | **PASS** | 580/580 TID matches, 0 orphans, per-CPU monotonic, tgid captured via raw_tp |

## Buffer Stress Test Results (Amendment 2)

Under `stress-ng --switch 128` (~8.6M events/sec, 32 CPUs):

| Finding | Recommendation |
|---------|---------------|
| perf_event_array 256KB/CPU (default) drops **7.4%** | **Minimum 1MB/CPU (256 pages)** |
| ringbuf 8MB = zero drops under extreme load | **16MB default is safe** |
| Poll interval 10ms vs 500ms = no difference at 32MB ringbuf | **100ms default** |
| ringbuf throughput ~6.5M/sec < perfbuf ~9M/sec | Shared spinlock overhead — perfbuf wins raw throughput |

**Phase 1 recommendation:** ringbuf 16MB, poll_timeout=100ms, with `--buffer-size` CLI override.

## Critical Discoveries Affecting Phase 0

1. **No cascade reference implementation exists.** bottleneck.py is an interactive SCC visualization tool, NOT a cascade engine. Analyzer.java's "cascade" is edge aggregation only. The Rust implementation will be the world's first production cascade with formal verification.

2. **Edge aggregation ≠ cascade redistribution.** Aggregation sums raw wait times; cascade recursively pushes blame to root causes. This is the fundamental insight enabling wPerf to find global bottlenecks (§11 of origin-analysis.md).

3. **`tests/fixtures/cascade_oracle.py` for non-overlapping graph validation.** The Python 3 script implements ADR-007 cascade pseudocode but lacks sweep-line partition (BUG-3). Valid only for graphs where each node has at most one overlapping outgoing dependency per time slice; complex overlapping graphs must use the Rust sweep-line implementation + invariant assertions. (Restored from tag `archived/gate0-prototypes`.)

4. **Buffer sizing is critical.** Default perf_event_array (256KB/CPU) drops 7.4% under extreme scheduler load. Production must use 1MB+/CPU for perfbuf or 16MB+ for ringbuf.

5. **raw_tp + BPF_CORE_READ validates ADR-013.** tgid is accessible from raw_tp context. Per-CPU ordering confirms ADR-004's perfarray analysis.

6. **libbpf-rs 0.26.1 API.** Requires `MaybeUninit<OpenObject>`, explicit trait imports (`SkelBuilder`, `OpenSkel`, `Skel`, `MapCore`), packed struct field copy before formatting, mutable `RingBufferBuilder` before `.build()`.

7. **Valuable wPerf-origin patterns.** `isFakeWake()` DPEvent state machine is more sophisticated than our 50μs threshold; thread grouping by stack similarity useful for Phase 3; per-CPU softirq tracking validates ADR-009 (§12 of origin-analysis.md).

## Gate 0 Passed. Phase 0 May Begin.
