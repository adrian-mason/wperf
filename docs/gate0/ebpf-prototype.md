# Gate 0 — eBPF Minimal Collection Prototype

- **Issue:** #7
- **Date:** 2026-03-22
- **Host:** kernel 6.18.19-1-lts, BTF available, clang 22.1
- **Pass criteria:** Matched switch/wakeup pairs by TID for 2-thread mutex workload

## Test Results

```
============================================================
Gate 0 #7: eBPF Minimal Collection Prototype
============================================================

[OK] BPF loaded and attached (raw_tp/sched_switch + raw_tp/sched_wakeup)
[OK] Workload: TID_A=314628, TID_B=314629
[..] Collecting 3 seconds...
[OK] Total events: 3,286,183
[OK] Workload: 1172 switches, 580 wakeups

--- Sample (first 3 switches) ---
  sw[0]: prev=0/0 next=314628/314627 ts=76873872939327
  sw[1]: prev=314628/314627 next=0/0 ts=76873872942612
  sw[2]: prev=0/0 next=314629/314627 ts=76873872944000
--- Sample (first 3 wakeups) ---
  wu[0]: waker=0/0 target=314628/314627 ts=76873872936748
  wu[1]: waker=0/0 target=314629/314627 ts=76873872942870
  wu[2]: waker=0/0 target=314628/314627 ts=76873877994361

--- Validation ---
  Matched:     580/580
  Orphans:     0
  Per-CPU mono: true
  tgid:        314627

[PASS] Events captured, TIDs match, per-CPU timestamps monotonic, tgid=314627
```

## Validation Summary

| Assertion | Result | Detail |
|-----------|--------|--------|
| Events captured | **PASS** | 3.3M total events in 3 seconds (~1.1M/sec) |
| Workload switch/wakeup | **PASS** | 1172 switches, 580 wakeups for the mutex workload |
| TID matching | **PASS** | 580/580 wakeups matched to subsequent switch (0 orphans) |
| Per-CPU monotonicity | **PASS** | Timestamps monotonic within each CPU (cross-CPU not guaranteed per ADR-004) |
| tgid captured | **PASS** | tgid=314627 via `BPF_CORE_READ(task, tgid)` — confirms ADR-013 raw_tp access |

## Architecture

```
BPF side (probe.bpf.c):
  raw_tp/sched_switch  → read prev/next task_struct via BPF_CORE_READ
  raw_tp/sched_wakeup  → read woken task + bpf_get_current_pid_tgid() for waker
  perf_event_array     → output via percpu staging heap

Rust side (main.rs):
  PerfBufferBuilder    → callback-based event collection
  Mutex workload       → 2 threads, 200 iterations each, 5ms hold time
  Matching logic       → pair wakeup(target=X) with next switch(next=X) by timestamp
```

## Discoveries

1. **3.3M events/sec throughput.** The system generated ~1.1M events/sec on this kernel. Production wPerf must handle this rate without drops.

2. **Per-CPU ordering confirmed.** perf_event_array delivers events in per-CPU order (monotonic within CPU), confirming ADR-004's analysis. Cross-CPU reordering requires the Min-Heap Reorder Buffer in the perfarray path.

3. **raw_tp provides full task_struct access.** `BPF_CORE_READ(prev, tgid)` works on raw_tp context, confirming ADR-013's decision to use raw_tp over standard tracepoints. Both `pid` (kernel TID) and `tgid` (process ID) are captured.

4. **waker identification in sched_wakeup.** The waker is obtained via `bpf_get_current_pid_tgid()` (the currently running task when the wakeup fires). This is correct for scheduler-mediated wakeups. For softirq-context wakeups, the waker would be a kernel thread — ADR-009's per-CPU softirq tracking is needed to attribute these.

5. **libbpf-rs 0.26.1 API.** Requires `MaybeUninit<OpenObject>` for skeleton open, `SkelBuilder`/`OpenSkel`/`Skel`/`MapCore` trait imports, and careful handling of packed struct field references (copy before format!). `RingBufferBuilder` requires mutable binding before `.build()`.

6. **Needs sudo.** `RLIMIT_MEMLOCK` bump requires root or `CAP_SYS_RESOURCE`. In production, `CAP_BPF` + `CAP_PERFMON` should suffice with `setrlimit` in the binary.

## Buffer Sizing (Amendment 2)

Stress test under `stress-ng --switch 128` (128 context-switch-heavy threads, ~8.6M events/sec on 32 CPUs):

### Phase 1: Buffer Size Sweep (poll_timeout=100ms)

| Test | Buffer Type | Buffer Size | Events/5s | Drops | Drop% |
|------|------------|-------------|-----------|-------|-------|
| A | perf_event_array | 256KB/CPU (default) | 42.9M | **3.4M** | **7.4%** |
| B | perf_event_array | 1MB/CPU | 44.9M | 0 | 0% |
| C | ringbuf | 8MB shared | 32.6M | 0 | 0% |
| D | ringbuf | 32MB shared | 31.7M | 0 | 0% |

### Phase 2: Poll Interval Sweep (ringbuf 32MB)

| Test | Poll Timeout | Events/5s | Drops | Drop% |
|------|-------------|-----------|-------|-------|
| E | 10ms (near real-time) | 32.9M | 0 | 0% |
| F | 100ms (default) | 33.0M | 0 | 0% |
| G | 500ms (batch) | 32.4M | 0 | 0% |

### Recommendations for Phase 1

1. **perf_event_array default (256KB/CPU) is unsafe** — 7.4% drop rate under extreme load. **Minimum 1MB/CPU (256 pages)** required.
2. **ringbuf 8MB is sufficient** even under extreme load (zero drops). 16MB is a conservative default.
3. **Poll interval has minimal impact** on drop rate at 32MB ringbuf. 100ms is a safe default — no benefit from more aggressive polling.
4. **ringbuf throughput (~6.5M/sec) is lower than perfbuf (~9M/sec)** — shared spinlock contention. For extreme workloads on high-core-count machines, perfbuf may deliver more events despite per-CPU overhead.
5. **For wperf record Phase 1:** default ringbuf 16MB, poll 100ms, with `--buffer-size` CLI override per ADR-004-supplement §E3.
