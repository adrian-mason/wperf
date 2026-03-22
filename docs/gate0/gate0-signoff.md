# Gate 0 — Signoff

- **Date:** 2026-03-22
- **Kernel:** 6.18.19-1-lts (x86_64, BTF available)
- **Toolchain:** clang 22.1, Rust 1.85+, libbpf-rs 0.24, Python 3.12

## Per-Issue Results

| Issue | Title | Result | Evidence |
|-------|-------|--------|----------|
| **#54** | wPerf-origin forensic analysis | **PASS** | 290-line analysis doc with 5 areas; critical discovery: no cascade reference implementation |
| **#8** | Python Cascade understanding | **PASS** | Figure 4: Parser=20ms, Network=80ms, total=100ms (exact match); SCC tests pass |
| **#9** | wPRF v1 roundtrip | **PASS** | 10-event roundtrip match; truncation recovery of 6/10 events; header = 64B exact |
| **#7** | eBPF minimal collection | **PASS** | 580/580 TID matches, 0 orphans, per-CPU monotonic, tgid captured via raw_tp |

## Critical Discoveries Affecting Phase 0

1. **No cascade reference implementation exists.** bottleneck.py is an interactive SCC visualization tool, NOT a cascade engine. Analyzer.java's "cascade" is edge aggregation. The Rust implementation will be the world's first production cascade with formal verification.

2. **run_figure4.py as minimal oracle.** The 80-line Python 3 script implementing ADR-007 pseudocode can serve as a differential testing oracle for Phase 0 (#20), replacing the non-existent bottleneck.py cascade.

3. **~1.1M events/sec throughput.** The host kernel generates 3.3M sched events in 3 seconds. Production ringbuf sizing must account for this rate.

4. **raw_tp + BPF_CORE_READ validates ADR-013.** tgid is accessible from raw_tp context. Per-CPU ordering confirms ADR-004's perfarray analysis.

5. **libbpf-rs 0.24 API quirks.** Requires `MaybeUninit<OpenObject>`, explicit trait imports (`SkelBuilder`, `OpenSkel`, `Skel`), packed struct field copy before formatting.

## Gate 0 Passed. Phase 0 May Begin.
