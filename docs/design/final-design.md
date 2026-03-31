# wPerf Final Design Specification

> **Status:** Approved for implementation
> **Date:** 2026-03-17
> **Scope:** Authoritative design — supersedes all prior spec documents

---

## Table of Contents

1. [Project Overview & Architecture](#1-project-overview--architecture)
2. [eBPF Probe Layer](#2-ebpf-probe-layer)
3. [Algorithm Pipeline](#3-algorithm-pipeline)
4. [Data Format — wPRF v1](#4-data-format--wprf-v1)
5. [Frontend & Visualization](#5-frontend--visualization)
6. [Verification Strategy](#6-verification-strategy)
7. [Implementation Plan — 16-Week Gated Phases](#7-implementation-plan--16-week-gated-phases)
8. [Risk Register](#8-risk-register)
9. [Appendix](#appendix)

---

## 1. Project Overview & Architecture

### 1.1 Goals & Scope

wPerf is an industrial-grade Rust reimplementation of the OSDI'18 wPerf paper. The core goal is to build a thread-level Wait-For Graph that uses cascade redistribution and Knot detection to precisely locate global performance bottlenecks in multi-threaded systems.

Unlike traditional off-CPU flame graphs, wPerf solves a fundamental problem: **local long waits do not equal global bottlenecks.** It uses graph-theoretic methods (SCC, condensation DAG, critical path) to identify true system-wide bottlenecks.

### 1.2 Unified CLI Model

> Decision rationale: [ADR-001: Offline CLI Model vs Backend Service](../decisions/ADR-001.md)

wPerf adopts a fully offline CLI tool model, aligning with the `perf record` / `perf report` user mental model:

```
wperf record [options]    # Collect: BPF probes → .wperf binary file
wperf report [options]    # Analyze: .wperf → algorithm pipeline → self-contained .html report
wperf version             # Version information
```

A single `wperf` binary (clap subcommands) guarantees version consistency. No long-running backend service is needed — the tool is fully offline. This follows the KUtrace offline processing philosophy.

### 1.3 Kernel Compatibility — Dynamic Feature Probing

> Decision rationale: [ADR-002: Dynamic Feature Probing vs Static Tiers](../decisions/ADR-002.md)

The kernel compatibility layer uses **per-feature dynamic probing at startup**, not static version-based tiers. The rationale: RHEL 8 (kernel 4.18) may backport BTF while a custom-compiled 5.x kernel might lack `CONFIG_DEBUG_INFO_BTF`; ringbuf (5.8+) and bpf_loop (5.17+) are orthogonal features that cannot be bundled into version-range tiers.

#### Startup Feature Probe Matrix

Using the libbpf ecosystem standard **probe → reconfigure → load** pattern, features are detected between skeleton `open()` and `load()`:

| Feature | Probe Method | Degradation |
|---------|-------------|-------------|
| **ringbuf** | `bpf_map_create(BPF_MAP_TYPE_RINGBUF)` | Reconfigure events map to `PERF_EVENT_ARRAY` (set key/value size) |
| **tp_btf** | `probe_tp_btf("sched_switch")` | Fall back to `raw_tp/sched_switch`; disable `tp_btf` programs via `set_autoload(false)` |
| **BTF (vmlinux)** | Check `/sys/kernel/btf/vmlinux` | Embedded BTF fallback; if still fails, refuse to run (`EOPNOTSUPP`) |
| **bpf_loop** | `libbpf_probe_bpf_helper(TRACEPOINT, BPF_FUNC_loop)` | `#pragma unroll` bounded loops |
| **cgroupv2** | Check `/sys/fs/cgroup/cgroup.controllers` | Disable cgroup filtering; `--cgroup` flag errors out |
| **tracepoint** | Check `/sys/kernel/tracing/events/{cat}/{name}` | `bpf_program__set_autoload(prog, false)` |
| **kprobe** | Blacklist + `available_filter_functions` | `set_autoload(false)` |
| **fentry** | Actual attach test | Fall back to kprobe/tracepoint |

When ringbuf **is** available, `set_autocreate(heap, false)` suppresses the unused percpu-array staging map. See [ADR-002-supplement](../decisions/ADR-002-supplement.md) and [ADR-004-supplement](../decisions/ADR-004-supplement.md) for empirical details.

#### Struct Field Version Differences (BPF CO-RE)

For known kernel struct field renames (e.g., `task_struct.state` → `task_struct.__state`, `bio.bi_disk` → `bio.bi_bdev`), use the bcc/libbpf-tools `core_fixes.bpf.h` pattern — versioned structs with `___old`/`___new` suffixes and `bpf_core_field_exists()` runtime branching:

```c
struct task_struct___old { volatile long int state; } __attribute__((preserve_access_index));
struct task_struct___new { unsigned int __state; }    __attribute__((preserve_access_index));

static __always_inline u32 get_task_state(void *task) {
    if (bpf_core_field_exists(struct task_struct___new, __state))
        return BPF_CORE_READ((struct task_struct___new *)task, __state);
    return BPF_CORE_READ((struct task_struct___old *)task, state);
}
```

This is resolved by the CO-RE compiler at load time with zero runtime overhead.

### 1.4 Minimum Kernel Version

> Decision rationale: [ADR-003: Minimum Kernel Version](../decisions/ADR-003.md)

- **Recommended minimum:** Linux 5.4 (RHEL 8.4+ / Ubuntu 20.04 LTS)
- **Best-effort support:** Linux 4.18 (RHEL 8.0+) via feature probing degradation
- 4.18's 4,096 BPF instruction limit doubles engineering complexity; RHEL 8.0–8.1 reached end of support in May 2024

### 1.5 Glossary

| Term | Definition |
|------|-----------|
| `WFG (Wait-For Graph)` | The core directed graph. Nodes are threads/pseudo-threads; edges represent causal waiting relationships (waiter → waitee). |
| `raw_wait_ms` | Direct observed physical wait time on a single edge before algorithmic processing. |
| `attributed_delay_ms` | Wait time after Cascade redistribution — reflects actual root-cause contribution to global delay. |
| `Condensation DAG` | A Directed Acyclic Graph formed by collapsing every SCC in the WFG into a single Super-node. |
| `Super-node` | A node in the Condensation DAG representing an entire SCC. Its weight uses the MAX heuristic of internal edges. |
| `Knot` | A Sink SCC (out-degree 0 in Condensation DAG) containing ≥1 user thread. Represents a true systemic bottleneck or deadlock cycle. |
| `Pseudo-thread` | A synthetic graph node representing a non-schedulable subsystem (e.g., disk I/O, softirq) for unified graph attribution. |
| `Spurious Wakeup` | A wakeup where the thread runs for < 50μs before sleeping again. Filtered out as noise edges. |
| `invariants_ok` | Boolean flag: true when structural postconditions hold — I-2 (non-amplification: `attributed ≤ raw` per edge) ∧ I-7 (locality: topology preserved). This is a structural guard, not a bug detector. See §3.4 and [ADR-016](../decisions/ADR-016.md). Replaces the deprecated `is_conserved` field. |

---

## 2. eBPF Probe Layer

### 2.1 Probe Points & Event Collection

#### Paper Alignment Table

> Decision rationale: [ADR-013: Scheduler Probe Type](../decisions/ADR-013.md)

Scheduler probes use `tp_btf` (BTF-enabled tracing, 5.5+) with `raw_tp` fallback (4.17+), not standard `tp/sched/`. This provides full `task_struct *` access for `tgid`, task flags, and cgroup traversal — fields absent from the standard tracepoint format struct. All libbpf-tools scheduler tools (offcputime, cpudist, runqslower, runqlat) use this pattern.

| Paper Probe | wPerf Probe (Primary) | wPerf Probe (Fallback) | Category |
|-------------|----------------------|----------------------|----------|
| `sched_switch` | `tp_btf/sched_switch` | `raw_tp/sched_switch` | **Core** (mandatory) |
| `sched_wakeup` | `tp_btf/sched_wakeup` | `raw_tp/sched_wakeup` | **Core** (mandatory) |
| `sched_wakeup_new` | `tp_btf/sched_wakeup_new` | `raw_tp/sched_wakeup_new` | **Core** (thread creation) |
| `sched_process_exit` | `tp_btf/sched_process_exit` | `raw_tp/sched_process_exit` | **Core** (cleanup) |
| — | `tp/syscalls/sys_enter_futex` | — | **Auxiliary** (wait cause) |
| — | `tp/block/block_rq_issue` + `block_rq_complete` | — | **Auxiliary** (IO attribution) |
| — | `tp/irq/softirq_entry` + `softirq_exit` | — | **Auxiliary** (softirq attribution) |

Non-scheduler probes (futex, block I/O, softirq) retain standard `tp/` — their trace event format structs provide all needed fields without `task_struct` traversal. Extensions beyond the paper are gated by `const volatile bool` feature flags, allowing selective enable/disable at load time.

### 2.2 Transport — Single-ELF CO-RE Dual-Mode

> Decision rationale: [ADR-004: Event Transport Strategy](../decisions/ADR-004.md)

A single BPF ELF object supports both ringbuf and perfarray transport, auto-detected at startup:

```
open() → probe_ringbuf() → if yes: set_autocreate(heap, false); use ringbuf maps
                          → if no:  set_type(events, PERF_EVENT_ARRAY); keep heap for staging
       → probe_tp_btf()  → if yes: set_autoload(raw_tp progs, false)
                          → if no:  set_autoload(tp_btf progs, false)
       → load() → attach()
```

This avoids maintaining two separate BPF programs. The `EventTransport` trait in userspace abstracts the polling interface. See [ADR-004-supplement](../decisions/ADR-004-supplement.md) for user-space implementation details.

#### Drop Detection

BPF-layer event loss must be tracked to provide the coverage metrics required by [ADR-012](../decisions/ADR-012.md):

- **Ringbuf path:** When `bpf_ringbuf_reserve()` returns NULL, a BPF-side `drop_counter` global is incremented. User-space reads this from the skeleton BSS section at the end of recording.
- **Perfarray path:** The `lost_cb` callback in `perf_buffer__new()` fires with the count of lost samples per overflow. User-space accumulates these counts.

Both paths expose a unified `drop_count` in the recording metadata, enabling users to assess data completeness.

### 2.3 Stack Unwinding — bpf_get_stackid + Elastic Stack Delta

> Decision rationale: [ADR-005: Stack Unwinding Approach](../decisions/ADR-005.md)

A two-layer approach:

**Fast path:** `bpf_get_stackid(ctx, &stack_map, BPF_F_USER_STACK)` — available since Linux 4.6, captures user-space call stacks when frame pointers are present. Low overhead (~100ns, 4 bytes stack ID per event), used on every sched_switch event.

**Deep path (Phase 3):** Elastic Stack Delta — rather than unwinding the full stack on every context switch, capture only the delta between consecutive stacks for the same thread. This approach is production-validated at Elastic on millions of nodes. The delta is resolved in userspace using DWARF debug info via blazesym.

When either path fails (fast path returns a suspiciously shallow stack ≤2 frames, or deep path encounters `bpf_probe_read_user` `-EFAULT`), the event is marked with `FLAG_PARTIAL_STACK` and `partial_stack_count` is incremented. Partial stacks are still usable for graph construction — only the flamegraph attribution is degraded.

### 2.4 BPF Event Structure

Fixed-length 23-byte BaseEvent wrapped in a 5-byte TLV (Type-Length-Value) header:

```
RecordHeader (5B): rec_type(u8) + length(u32)
BaseEvent   (23B): event_id(1) + cpu(2) + pid(4) + tid(4) + timestamp_ns(8) + flags(4)
```

Variable-length PayloadChunk: `tid(4) + data(N)`, optional Zstd compression. Maximum 16MB per chunk (DoS protection).

### 2.5 Spurious Wakeup Filtering

Spurious wakeups (where a thread is woken but immediately goes back to sleep) generate noise edges in the Wait-For Graph. These are filtered by checking:
1. The woken thread's actual running duration after wakeup
2. If duration < configurable threshold (default: 50μs), the wakeup is classified as spurious
3. Spurious wakeup edges are excluded from cascade redistribution

Each filtered wakeup increments `false_wakeup_filtered_count`, providing visibility into how aggressively the filter is pruning. A high count relative to total wakeups may indicate the threshold needs tuning.

---

## 3. Algorithm Pipeline

### 3.1 Seven-Step Pipeline

The algorithm processes collected events through 7 steps in strict order:

```
Step 1: Event Parsing & Reordering (timestamp-sorted state machine)
Step 2: Noise Edge Pruning (filter edges below threshold)
Step 3: Synthetic Edge Injection (IO pseudo-threads)
Step 4: Cascade Engine (weight redistribution)
Step 5: Tarjan SCC + Condensation (find strongly connected components)
Step 6: Max Heuristic Sorting (rank bottlenecks)
Step 7: Critical Path DP (find worst-case delay chain)
```

### 3.2 Event Parsing & Reordering

A 4-step finite state machine correlates raw BPF events into graph edges:

1. **Parse:** Deserialize TLV records from .wperf file, validate checksums
2. **Reorder:** Min-heap reorder buffer (1024 slots) compensates for per-CPU timestamp skew
3. **Correlate:** Match sched_switch → sched_wakeup pairs by TID to form (waiter, holder, duration) edges
4. **Orphan handling:** Wakeup events without a matching switch are logged, discarded, and tracked via `unmatched_wakeup_count`

### 3.3 Synthetic Edge Injection

> Decision rationale: [ADR-009: IO Attribution via Synthetic Edges](../decisions/ADR-009.md)

For I/O and softirq delays, synthetic pseudo-thread nodes are injected into the graph:

- **Block I/O:** A `block_device:<dev>` pseudo-thread is created. `block_rq_issue` creates an edge from the requesting thread to the pseudo-thread; `block_rq_complete` creates the return edge.
- **softirq:** A `softirq:<vec>` pseudo-thread is created for each softirq vector (NET_RX, BLOCK, TIMER, etc.). The `softirq_entry`/`softirq_exit` tracepoints track the active softirq vector per CPU. When a `sched_wakeup` fires within a tracked softirq context (e.g., `BLOCK_SOFTIRQ` or `NET_RX_SOFTIRQ`), the wakeup edge is attributed to the corresponding subsystem pseudo-thread (Disk or Network), bridging the causal gap between hardware interrupt completion and user-thread execution.

Pseudo-threads participate in cascade redistribution and can appear in Knots, enabling attribution of delays to hardware/subsystem bottlenecks.

### 3.4 Cascade Engine

> Decision rationale: [ADR-007: Cascade Verification Strategy](../decisions/ADR-007.md)

The cascade algorithm redistributes wait time from direct waiters to root-cause holders. When thread A waits for thread B, and B is itself waiting for C, B's wait time should be attributed to C (the root cause), not counted as B's own bottleneck contribution.

**Algorithm:** Depth-first traversal from each node, proportionally redistributing wait time along dependency chains. Key implementation details:

- `path.insert` / `path.remove` outside the recursion loop (BUG-1 fix)
- `child_subtree_absorbed = propagated_down + child_self_blame` (NEW-BUG-1 fix: leaf nodes must not have zero blame)
- `sweep_line_partition` for O(N log N) time-slice partitioning ensuring weight conservation
- Maximum recursion depth of 10 (practical limit for real workloads); if exceeded, the branch is truncated and `cascade_depth_truncation_count` is incremented
- Complexity: O(E × D × log K) where D=recursion depth ≤10, K=concurrent holders typically <5, effectively near-linear

**Six invariants** verified across multiple layers (see [ADR-016](../decisions/ADR-016.md) for I-1 retirement, I-6 narrowing, and the verification coverage matrix):

| ID | Invariant | What It Catches | Scope |
|----|-----------|----------------|-------|
| **I-2** | Non-amplification: no edge's attributed_delay > its raw_wait | Double-counting errors | All graphs |
| **I-3** | Non-negativity: all attributed_delay ≥ 0 | Sign errors in redistribution | All graphs |
| **I-4** | Termination: cascade completes within bounded steps | Infinite recursion from cycle handling bugs | All graphs |
| **I-5** | Idempotency: running cascade twice produces identical results | Non-deterministic state leaks | All graphs |
| **I-6** | Depth monotonicity: deeper recursion redistributes less weight | Incorrect proportional allocation | **Simple chains only** |
| **I-7** | Locality: weight flows only along existing edges, bounded by time window intersection | Path traversal errors (BUG-1 class) | All graphs |

**I-1 (conservation) retired ([ADR-016](../decisions/ADR-016.md)):** The per-entry-edge subtree-sum conservation definition from [ADR-015](../decisions/ADR-015.md) was proven incorrect for fan-out graphs. Per-edge independent cascade has no cross-edge conservation invariant — each edge is cascaded independently with no shared conservation pool.

**I-6 qualified ([ADR-016](../decisions/ADR-016.md)):** Depth monotonicity holds only for simple chains (no fan-out, no concurrent waiters). With fan-out, integer division in proportional allocation causes `child_absorbed < window.duration()`, so deeper recursion may propagate less weight than the truncation base case. I-6 is verified by unit tests on simple chains and excluded from property-based testing on random graphs.

**Production sentinel:** The `invariants_ok` boolean flag (I-2 ∧ I-7) is validated at runtime in ALL builds (release + debug) and exported in JSON output, serving as the production-level structural postcondition guard per [ADR-007](../decisions/ADR-007.md) and [ADR-016](../decisions/ADR-016.md). **Limitation:** I-2 is unfalsifiable by construction through the current cascade path (`attributed = raw_wait.saturating_sub(propagated)` guarantees `attributed ≤ raw`). The sentinel catches 0/5 known bugs — its value is defense-in-depth against future regressions, not bug detection. Known-bug coverage comes from the full verification stack (regression tests, proptest, differential oracle, mutation testing). I-3 and I-4 are enforced via `debug_assert!` only (stripped in release builds). I-5 (idempotency) requires running cascade twice and is checked in debug builds only.

### 3.5 Tarjan SCC + Condensation + Knot Detection

Using `petgraph::algo::tarjan_scc`:

1. Find all strongly connected components
2. Build condensation graph (each SCC becomes a super-node in a DAG)
3. Filter sink SCCs (out_degree == 0)
4. Business filtering:
   - Exclude trivial sinks (|SCC|==1, no self-loop)
   - Exclude pure kernel-thread SCCs
5. Remaining sink SCCs containing at least one userspace worker thread are **Knots**

Note: `tarjan_scc` returns reverse topological order (sink SCCs first).

### 3.6 Max Heuristic Sorting

> Decision rationale: [ADR-008: SCC Weight Heuristic](../decisions/ADR-008.md)

Super-node weight in the condensation DAG = **MAX()** of all relevant edge weights within the SCC.

This is explicitly a **sorting heuristic for bottleneck ranking**, not a mathematically rigorous representation. SCC blocking relationships may be sequential causal chains rather than concurrent — the MAX approximation preserves relative ordering for practical bottleneck prioritization.

### 3.7 Critical Path DP

On the acyclic condensation DAG, O(V+E) dynamic programming via topological sort extracts the maximum-weight path — the critical delay chain through the system.

Topological sort is undefined on cyclic graphs (would skip cycle nodes or panic), which is why it **must** operate on the condensation DAG, not the original graph.

### 3.8 "No False Negatives" Claim Disposition

> Decision rationale: [ADR-012: "No False Negatives" Guarantee](../decisions/ADR-012.md)

wPerf makes **no mathematical guarantee of zero false negatives.** Three real-world conditions break complete coverage:

1. **Ring buffer overflow:** Events may be dropped under extreme load (no backpressure)
2. **Clock skew:** NTP adjustments can cause timestamp ordering violations
3. **Sampling granularity:** Context switch tracing has inherent resolution limits

Instead, wPerf provides a **practical coverage assurance** with quantified observability of its own coverage gaps. Five metrics are tracked and exported in the recording metadata and JSON output:

| Metric | Source | What It Measures |
|--------|--------|-----------------|
| `drop_count` | BPF probe layer (§2.2) | Events lost to ring buffer overflow or perfarray overflow |
| `unmatched_wakeup_count` | Event correlation (§3.2) | Wakeup events with no matching sched_switch — incomplete causal edges |
| `partial_stack_count` | Stack unwinding (§2.3) | Events where stack capture failed or returned ≤2 frames |
| `cascade_depth_truncation_count` | Cascade engine (§3.4) | Causal chains truncated at depth 10 — potential missed deep attribution |
| `false_wakeup_filtered_count` | Spurious wakeup filter (§2.5) | Wakeup edges pruned as noise — indicates filter aggressiveness |

These metrics transform the tool from a black-box oracle into a transparent instrument: users can assess data completeness and calibrate confidence in the results.

---

## 4. Data Format — wPRF v1

> Decision rationale: [ADR-010: Binary Format Design](../decisions/ADR-010.md)

### 4.1 64-Byte File Header

```rust
pub struct WprfHeader {
    pub magic: [u8; 4],                // "wPRF" (4B)
    pub version: u8,                   // 1 (1B)
    pub endianness: u8,                // 1=LE (1B)
    pub host_arch: u8,                 // 0=x86_64, 1=aarch64 (1B)
    pub _reserved: u8,                 // Formerly meta_flags, retained for 64B alignment (1B)
    pub data_section_end_offset: u64,  // Crash recovery offset (8B)
    pub section_table_offset: u64,     // Points to footer Section Table (8B)
    pub feature_bitmap: [u8; 32],      // 256-bit Feature Flags (32B)
    pub reserved_padding: [u8; 8],     // Align to 64B (8B)
}
```

The `version` field and TLV record format enable forward compatibility — readers can skip unknown record types without failing.

### 4.2 TLV Event Stream

Each event is wrapped in a `RecordHeader` (5B): `rec_type(u8) + length(u32)`.

- **BaseEvent** (fixed 23B): event_id + cpu + pid + tid + timestamp_ns + flags
- **PayloadChunk** (variable): tid + data, optional Zstd compression. Max 16MB (DoS protection).

### 4.3 Footer Section Table

Stream-friendly design — written at the end of the file:

| Section ID | Content |
|-----------|---------|
| 1 | String Table (thread names, cgroup names) |
| 2 | Symbol Resolution Table |
| 3 | Metadata (Build-ID, HOSTNAME, OSRELEASE, CLI_ARGS, DROP_COUNT) |
| 4 | Attr Section (Collection config, sampling rates — P2 deferred) |

### 4.4 Crash Tolerance

The `data_section_end_offset` in the header enables forward-scanning recovery when the footer is missing (collection interrupted by SIGKILL or power loss). This exceeds perf.data's crash tolerance.

### 4.5 Compression Ratio

Zstd standard entropy (including high-entropy nanosecond timestamps): actual compression ratio **5:1 to 8:1**. The prior 20:1 estimate was overly optimistic — capacity planning uses 5:1 as baseline.

---

## 5. Frontend & Visualization

### 5.1 Fully Offline Single-File HTML

The `wperf report` command produces a self-contained `.html` file with all resources Base64-inlined. No CDN dependencies, no network access required. Opens in any modern browser.

### 5.2 Dagre Layout + ECharts Rendering

> Decision rationale: [ADR-006: Graph Visualization](../decisions/ADR-006.md)

**Layout:** Dagre (Sugiyama hierarchical algorithm) — produces top-to-bottom directed graph layouts that naturally express "who waits for whom" semantics. Dagre computes (x, y) coordinates for each node.

**Rendering:** ECharts with `layout: 'none'` — uses Dagre-computed coordinates for absolute positioning. ECharts provides interaction (zoom, pan, hover tooltips, click-to-focus) without reimplementing a rendering engine.

This is a **joint solution:** Dagre handles layout correctness, ECharts handles rendering and interaction. Neither alone solves the problem — force-directed layouts lose directionality; ECharts' built-in layouts lack Sugiyama quality.

### 5.3 Visual Encoding

| Element | Condition | Style |
|---------|-----------|-------|
| Edge | attributed_delay > 500ms | Red, thick (3px) |
| Edge | 50ms ≤ attributed_delay ≤ 500ms | Black, solid (1.5px) |
| Edge | attributed_delay < 50ms | Gray, dashed (0.5px) |
| Node | Part of a Knot | Red border box |
| Node | Bottleneck (top-ranked) | Highlighted fill |
| Node | Pseudo-thread | Distinct shape (diamond) |

Noise edges (< 50ms, typically 67% of all edges based on prototype data analysis) are rendered as barely-visible gray dashes, dramatically reducing visual clutter without hiding information.

**Self-loop handling:** As established in [ADR-006](../decisions/ADR-006.md), Dagre hierarchical layout does not naturally support self-loops (A → A edges). The frontend must intercept self-loop edges and render them using custom loop-back arrows (Bézier curves) in ECharts to prevent rendering artifacts.

**Data presentation constraints ([ADR-008](../decisions/ADR-008.md)):** When displaying the weight of a condensed Knot (SCC Super-node) in tooltips or detail panels, the UI **must not** label it as "Total Delay" or "Bottleneck Cost". It must be explicitly labeled as "Max Internal Wait" to reflect its nature as a sorting heuristic rather than an absolute temporal sum.

### 5.4 Inferno Flamegraph

Stack traces collected via bpf_get_stackid are rendered as interactive SVG flamegraphs using the `inferno` crate. The flamegraph is embedded in the same HTML report, linked from the relevant graph node.

### 5.5 Coverage & Health Dashboard

To satisfy the transparency requirements of [ADR-012](../decisions/ADR-012.md) and the production sentinel of [ADR-007](../decisions/ADR-007.md), the HTML report **must** include a prominent health dashboard displaying:

- **Algorithm health:** `invariants_ok` boolean flag — structural postcondition guard (I-2 non-amplification ∧ I-7 locality). A `false` value indicates a Cascade Engine structural violation and the results should not be trusted. See [ADR-016](../decisions/ADR-016.md)
- **Data completeness metrics:** `drop_count`, `unmatched_wakeup_count`, `partial_stack_count`, `cascade_depth_truncation_count`, `false_wakeup_filtered_count`

This ensures users can calibrate their confidence in the analysis based on empirical data quality, rather than relying on an unverifiable guarantee.

---

## 6. Verification Strategy

### 6.1 Five-Layer Verification Pyramid

> Decision rationale: [ADR-007: Cascade Verification Strategy](../decisions/ADR-007.md)

| Layer | What | When | Target |
|-------|------|------|--------|
| **L1: Invariant Checks** | `invariants_ok` (I-2 ∧ I-7) in all builds; I-3, I-4 via `debug_assert!`; I-5, I-6 in tests ([ADR-016](../decisions/ADR-016.md)) | Day 1 | Per coverage matrix |
| **L2: Paper Scenarios + Regressions** | Figure 4 + 5 known bug regressions | Week 1 | 10+ hardcoded test cases |
| **L3: Property-Based Testing** | proptest random graph generation | Week 2 | 10,000+ random topologies |
| **L4: Differential Testing** | Rust vs Python cascade oracle (`cascade_oracle.py`, ADR-007 pseudocode) | Week 2-3 | ≤1.0ms tolerance |
| **L5: Mutation Testing** | cargo-mutants kill rate | Week 3 | ≥90% mutation detection |

### 6.2 Production Sentinel + Invariants

The production sentinel `invariants_ok` (I-2 non-amplification ∧ I-7 locality) is a structural postcondition guard that runs in all builds. It catches 0/5 known bugs by construction (I-2 is unfalsifiable via `saturating_sub`; I-7 checks topology only). Known-bug detection relies on the full verification stack: regression tests, proptest, differential oracle, and mutation testing. See [ADR-016](../decisions/ADR-016.md).

I-7 (locality) catches path traversal errors where weight flows to non-adjacent nodes. I-3 and I-4 are enforced via `debug_assert!` in debug builds. I-5 (idempotency) and I-6 (depth monotonicity, simple chains only) are verified in tests.

### 6.3 Four Paper Scenarios

| Scenario | Input | Core Assertion |
|----------|-------|---------------|
| **mysql_lock** | 2-thread mutex | Knot exists with ≥2 SCC nodes; max_penalty_ms ≥ 80% of baseline |
| **hbase_flush** | IO-intensive | IO pseudo-thread attributed_delay ≥ 90% of total |
| **memcached_spin** | Spin contention | < 50μs edges pruned; total edge count drops by order of magnitude |
| **apache_net** | Network event-driven | Network pseudo-thread in top-3 bottlenecks |

### 6.4 Differential Testing + Common-Mode Mitigation

The Python cascade oracle (`cascade_oracle.py`, implementing the ADR-007 pseudocode independently) serves as the differential testing reference. Note: the original wPerf repository's `bottleneck.py` does NOT implement cascade redistribution — it is an interactive SCC visualization tool that reads pre-aggregated edge weights (see Gate 0 finding, [cascade-understanding.md](../gate0/cascade-understanding.md) §1). The same synthetic input graph (constructed entirely in userspace without BPF dependencies, per the [ADR-011](../decisions/ADR-011.md) Phase 0 isolation constraint) is processed by both Rust and Python implementations; results must agree within ≤1.0ms tolerance (floating-point precision).

**Common-mode failure risk:** Both implementations derive from the same ADR-007 pseudocode, so they could share the same conceptual error. Mitigation:
- Cross-reference with the OSDI'18 paper's Figure 4 expected outputs (independent ground truth)
- The 6 invariants (I-2 through I-7, see [ADR-016](../decisions/ADR-016.md)) provide independent structural verification
- Test with adversarial inputs designed to expose specific algorithm edge cases
- The Python oracle is restricted to non-overlapping graphs (no sweep-line partition); complex topologies rely on invariant assertions (I-2 through I-7) in the Rust implementation (see [cascade-understanding.md](../gate0/cascade-understanding.md) §6.3)

### 6.5 Mutation Testing

`cargo-mutants` systematically modifies the Cascade Engine code (delete operations, flip conditions, change constants) and verifies that the test suite detects each mutation. Target: ≥90% kill rate.

Key mutation targets: deletion of `path.insert`, modification of duration calculations, flipping proportional allocation, changing depth limits, deletion of `coverage_duration`, modification of `child_subtree_absorbed`.

### 6.6 Cross-Kernel E2E

**Tool:** virtme-ng (second-scale cross-kernel boot)
- Extract vmlinuz + `/lib/modules/` from distro RPM/deb packages
- Test kernels: 4.18 (best-effort baseline) / 5.4 (recommended minimum) / 5.8 / 5.17 / 6.x (representing key BPF capability boundaries)
- Each kernel version validates the feature probe → degradation → load path
- Testing on 4.18 is strictly required to validate the feature degradation paths (perfarray fallback, `#pragma unroll` fallback, `raw_tp` fallback). Without it, the entire degradation architecture designed in ADR-002, ADR-004, and ADR-013 is unexercised dead code

### 6.7 Performance Overhead Benchmarking

To satisfy the overhead constraints in the Phase Exit Gates (§7.3), profiling overhead must be validated under automated, reproducible conditions:

- **Workload:** `stress-ng --matrix 64` (or equivalent multi-threaded CPU/scheduler stressor) to generate >100K sched_switch events/sec
- **Measurement:** `wperf record` process CPU utilization via `pidstat -p <pid> 1` over a 60-second collection window
- **Thresholds:** < 3% single-core equivalent CPU usage for base collection (Phase 1 gate); < 5% with user-stack collection enabled (Phase 3 gate)
- **Environment:** Same virtme-ng kernel matrix as §6.6; overhead must meet threshold on all test kernels

---

## 7. Implementation Plan — 16-Week Gated Phases

> Decision rationale: [ADR-011: Phase Structure](../decisions/ADR-011.md)

### 7.1 Gate 0: Three Prototype Validations (Week 1)

Three throwaway prototypes validate high-risk assumptions before any production code is written. All prototype code is discarded.

| Prototype | Validates | Pass Criteria | Failure Means |
|-----------|----------|---------------|---------------|
| **A: eBPF Minimal Collection** | sched_switch + sched_wakeup can capture matching event pairs on the host kernel | Event pairs match by TID for a 2-thread mutex workload | Toolchain or kernel issue — Phase 1 cannot start |
| **B: Python Cascade** | Complete understanding of the paper's cascade redistribution algorithm | Figure 4 output matches paper exactly (Network=80ms, Parser=20ms, chain sum: 20+80=100ms) | Cascade understanding is wrong — Rust implementation would diverge |
| **C: wPRF Roundtrip** | 64B header + TLV can serialize/deserialize/crash-recover | 10-event roundtrip + truncation recovery of first N events | Format spec has ambiguity — fix before coding |

### 7.2 Phase 0–3 Detailed Timeline

```
Gate 0 (Prototype Validation)                  Week 1
├── A: eBPF minimal collection                  2 days
├── B: Python Cascade understanding             1 day
└── C: .wperf roundtrip                        2 days

Phase 0 (Cascade Correctness)                  Weeks 2–4
├── W1: Core implementation + invariants (I-2~I-7, ADR-016) + Figure 4 tests
├── W2: proptest 10K graphs + 5 bug regressions + SCC/Condensation
└── W3: Differential vs Python + mutation testing

Phase 1 (Minimal Data Pipeline)                Weeks 5–8
├── W1: wperf record (BPF probes + .wperf format)
├── W2: wperf report (state machine + graph construction)
├── W3: Pipeline integration + JSON output + minimal visualization (dot SVG)
└── W4: E2E testing + overhead baseline + crash recovery

Phase 2a (Wait Cause Annotation)               Weeks 9–10
└── sys_enter_futex + spurious wakeup filtering (no graph topology change)

Phase 2b (Synthetic Edges + Attribution)        Weeks 11–12
└── block_rq + softirq pseudo-threads (changes graph topology)

Phase 3 (Stack Collection + Production HTML)    Weeks 13–16
├── W1-2: bpf_get_stackid + Elastic Stack Delta porting + symbol resolution (blazesym)
├── W3: Dagre + ECharts HTML report
└── W4: Flamegraph + cross-kernel testing
```

### 7.3 Phase Exit Gate Criteria

| Gate | Must-Pass Criteria | Method |
|------|-------------------|--------|
| **Gate 0** | A: matched switch/wakeup pairs; B: Figure 4 exact match; C: 10-event roundtrip + truncation recovery | Manual |
| **Phase 0** | `invariants_ok` (I-2 ∧ I-7) 0 violations on all test graphs; I-2 through I-5, I-7 verified by 10K proptest with 0 violations; I-6 verified by unit tests on simple chains; 5 bug regressions pass; vs Python ≤1.0ms (6 fixed scenarios); mutation ≥90% ([ADR-016](../decisions/ADR-016.md)) | **Automated** |
| **Phase 1** | 2-thread mutex Knot detected; `invariants_ok==true` on real BPF data; all 5 coverage metrics exported in JSON; overhead <3% CPU (stress-ng 64 threads); crash recovery passes; minimal SVG readable | Automated + manual review |
| **Phase 2a** | Correct futex wait_type annotation; spurious wakeups filtered; `invariants_ok` preserved | Automated |
| **Phase 2b** | IO pseudo-thread `attributed_delay ≥ 70%`; no spurious Knots from synthetic edges; `invariants_ok` preserved | Automated + manual review |
| **Phase 3** | Stack depth ≥ 5 frames (with FP); Dagre layout renders; flamegraph functions readable; total overhead < 5% CPU | Automated + manual |

### 7.4 Three Prohibitions + Rollback Strategy

**Three prohibitions** (learned from the archived project's failure):

1. **Never modify specs/assertions to match code output.** If tests fail, the bug is in the code, not the spec.
2. **Never add Phase N+1 workarounds to bypass Phase N problems.** Fix at the source.
3. **Never develop two Phases concurrently.** Sequential execution prevents integration explosion.

**Rollback procedure:** Phase N+1 discovers a Phase N bug → add regression test in Phase N suite → fix in Phase N code → Phase N gate must re-pass → then resume Phase N+1.

---

## 8. Risk Register

### 8.1 Risk 1: Cascade Engine Complexity — HIGH

The Cascade Engine has no third-party production implementation worldwide (the only reference is the original Python prototype, ~51 GitHub stars). Three rounds of pseudocode review found 5 bugs before any code existed. The `sweep_line_partition()` function still lacks pseudocode. While the verification strategy (§6) is now codified, its effectiveness in a production Rust implementation remains unproven.

**Mitigation:** Five-layer verification pyramid (§6.1); Phase 0 dedicated to algorithm correctness with 3-week timeline; petgraph handles SCC/condensation (3.6K stars, mature library).

### 8.2 Risk 2: MVP Scope — HIGH

The full 7-step pipeline + 6 probe types + wPRF format + Dagre+ECharts UI is ambitious. The original paper used only 2 probes + Python analyzer + CSV output.

**Mitigation:** 16-week gated phases with strict serial execution; each phase has a clear scope boundary and exit gate; Gate 0 validates assumptions before investment.

### 8.3 Risk 3: Minimum Kernel Version — MEDIUM

Supporting kernel 4.18 (RHEL 8.0) doubles engineering complexity due to the 4,096 BPF instruction limit, lack of `bpf_probe_read_kernel()`, and absent BTF/CO-RE. Per [ADR-002-supplement](../decisions/ADR-002-supplement.md), native 4.18 kernels (RHEL 8.0-8.1) lack BTF entirely — without implementing the embedded BTF (`min_core_btfs`) pattern, the tool will refuse to run (`EOPNOTSUPP`) on these kernels despite other feature degradations being available.

**Mitigation:** 5.4 recommended minimum (BTF available in all major distributions); 4.18 best-effort requires a Phase 1 scope decision on whether to ship embedded BTF or accept BTF as a hard requirement; explicit degradation matrix documents what works on each kernel.

### 8.4 Risk 4: AI-Generated Spec Blind Spots — MEDIUM

The design review process (22 documents across multiple AI models) exhibited 4 systemic biases: false closures (59.4% of findings dropped in first response round), incomplete attribution (single-cause diagnoses for multi-factor problems), solution drift (5 versions of stack unwinding), and authority inflation (claims introduced with unwarranted certainty). There may be undetected blind spots remaining.

**Mitigation:** Gate 0 prototype validation; each phase gate provides empirical check against design assumptions; regression tests lock in validated behavior.

### 8.5 Risk 5: Technology Dependency Maturity — MEDIUM

- **dagre.js:** GitHub repository has been inactive; may need to fork or find alternative maintainer
- **Elastic Stack Delta:** Porting from Go/Java ecosystem to Rust; effort not yet estimated
- **inferno crate:** Flamegraph rendering; compatibility not yet evaluated

**Mitigation:** Phase 1 minimal visualization (dot/SVG) avoids dagre dependency until Phase 3; Stack Delta deferred to Phase 3; inferno evaluation in Phase 3 Week 3.

### 8.6 Risk 6: Differential Testing Common-Mode Failure — LOW

Both Rust implementation and Python oracle could share the same conceptual misunderstanding of the paper.

**Mitigation:** Paper Figure 4 as independent ground truth; adversarial test inputs; explicit documentation of any Python divergences.

### 8.7 Risk 7: "Zero P0/P1" is a Design Claim, Not an Implementation Guarantee — LOW

The prior design spec claimed "zero P0/P1 residual items." This was a design completeness statement built on zero lines of production code, zero automated tests, zero BPF programs loaded.

**Mitigation:** Gate 0 + phase gates convert paper claims into empirical validation at each milestone.

### 8.8 Risk 8: Clock Skew & Timestamp Non-monotonicity — MEDIUM

`bpf_ktime_get_ns()` uses `ktime_get_mono_fast_ns()`, which the kernel documentation explicitly states is "not guaranteed to be monotonic across an update" during NTP/PTP clock slope adjustments. Timestamp inversions can cause the event correlation state machine to mismatch sched_switch and sched_wakeup events, producing incorrect `raw_wait` edges or O(N²) reorder buffer churn.

**Mitigation:** 50μs false-wakeup filter absorbs micro-inversions (NTP-induced skew is typically nanosecond-to-low-microsecond on modern x86 with `constant_tsc` + `nonstop_tsc`); `unmatched_wakeup_count` metric quantifies correlation failures; ringbuf path provides global ordering that eliminates cross-CPU timestamp issues; perfarray path's Min-Heap Reorder Buffer (50ms window) tolerates larger inversions.

### 8.9 P2 Deferred Items

| Item | Status | Disposition |
|------|--------|-------------|
| **wPerf-origin git history review** | Deferred since early reviews, never executed | **Mandatory pre-Phase 0 prerequisite** — archived project's failure patterns are critical design input |
| Synthetic edge weight calculation details | P2 | Complete before Phase 2b |
| Attr Section + Header byte layout | Design codified (Section ID 4 reserved in §4.3), implementation P2 | Complete before Phase 1 W1 |
| User-space annotation (#11) | V1 (futex only) | Phase 4+ — does not block v0.1.0-alpha |
| CI simplification (#6) | V0 (never discussed) | Phase 0-1: `cargo test` only; Phase 2+: define minimal CI |
| cgroupv1 cgroup_id alternative | P3 | Handle when cgroupv2 probe fails |

---

## Appendix

### A. Decision Record Index

| ADR | Decision | Chosen Option |
|-----|----------|--------------|
| [ADR-001](../decisions/ADR-001.md) | Architecture model | Offline CLI (record/report) |
| [ADR-002](../decisions/ADR-002.md) | Kernel compatibility | Dynamic per-feature probing |
| [ADR-003](../decisions/ADR-003.md) | Minimum kernel version | 5.4 recommended, 4.18 best-effort |
| [ADR-004](../decisions/ADR-004.md) | Event transport | Single-ELF CO-RE dual-mode |
| [ADR-005](../decisions/ADR-005.md) | Stack unwinding | bpf_get_stackid + Elastic Stack Delta |
| [ADR-006](../decisions/ADR-006.md) | Graph visualization | Dagre layout + ECharts rendering |
| [ADR-007](../decisions/ADR-007.md) | Cascade verification | Five-layer verification pyramid |
| [ADR-008](../decisions/ADR-008.md) | SCC weight heuristic | MAX as sorting heuristic |
| [ADR-009](../decisions/ADR-009.md) | IO attribution | Synthetic edges with pseudo-threads |
| [ADR-010](../decisions/ADR-010.md) | Binary data format | Clean wPRF v1 with TLV + crash tolerance |
| [ADR-011](../decisions/ADR-011.md) | Phase structure | 16-week gated phases |
| [ADR-012](../decisions/ADR-012.md) | "No false negatives" | Explicit retraction, pragmatic assurance |
| [ADR-013](../decisions/ADR-013.md) | Scheduler probe type | tp_btf + raw_tp fallback |
| [ADR-014](../decisions/ADR-014.md) | libbpf/CO-RE best practices | Nakryiko reference alignment |
| [ADR-002-supplement](../decisions/ADR-002-supplement.md) | Kernel compat empirical data | libbpf-tools code analysis |
| [ADR-004-supplement](../decisions/ADR-004-supplement.md) | Transport user-space details | Dual-mode polling lifecycle |

### B. Changes from Prior Design

| Aspect | Prior | Current | Rationale |
|--------|-------|---------|-----------|
| Risk 1 (Cascade) severity | Medium | **High** | 5 bugs found in pseudocode alone; specification itself is high-error-surface |
| Phase 0 duration | 2 weeks | **3 weeks** | Added differential testing + mutation testing time |
| Phase 2 structure | Monolithic | **Split into 2a/2b** | Isolate graph topology changes (2b) from annotation-only changes (2a) |
| Phase 1 visualization | None | **Minimal SVG** | Avoid "can't distinguish algorithm bug from rendering bug" |
| Gate 0 prototypes | None | **3 prototypes** | Validate high-risk assumptions before committing to implementation |
| Additional risks | 3 risks | **8 risks** | 5 omissions identified by pre-execution audit and cross-ADR review |
| Decision rationale | Mixed into body | **Separate ADR files** | Clean separation of "what" from "why" |
| "No false negatives" | Silently disappeared | **Explicit disposition** | Honest characterization of coverage guarantees |
| I-7 invariant | Not present | **Adopted** | Catches path traversal errors (BUG-1 class); now part of `invariants_ok` production sentinel ([ADR-016](../decisions/ADR-016.md)) |

### C. Archived Project Failure Patterns

Quantified evidence from the archived Rust implementation (462 commits, Feb 2024 – Feb 2026):

| Pattern | Evidence |
|---------|---------|
| **S-curve collapse** | Feb 15-23: near-0% fix rate → Feb 26-28: 59-88% fix rate |
| **Final day meltdown** | 17 commits, 15 fixes (88%) |
| **Full-stack concurrent changes** | FACB four-layer single commits touching algorithm+types+frontend+performance |
| **Core algorithm instability** | cascade.rs: 44 modifications (1,507 lines); knot.rs: 33 modifications |
| **Spec reverse-modification** | 9 instances of relaxing/aligning/removing test assertions in 7 days |
| **Context overflow** | 8 pairs of commits with identical messages (AI agent lost context, redid work) |
| **Binary format churn** | 5 format versions (wPRF1→wPRF5) with no forward compatibility |
| **Fix ratio** | 198/462 commits (42.8%) were fixes; fix-to-feature ratio 1.51 |

These patterns directly inform the three prohibitions in § 7.4 and the gated phase structure in § 7.2.

### D. External References

| Resource | Relevance |
|----------|-----------|
| [OSDI'18 wPerf Paper](https://www.usenix.org/conference/osdi18/presentation/yu) | Authoritative algorithm specification (Cascade, Knot, WFG) |
| [Nakryiko Blog (nakryiko.com)](https://nakryiko.com/) | libbpf maintainer's CO-RE, ringbuf, and portability best practices (ADR-014) |
| [bcc/libbpf-tools](https://github.com/iovisor/bcc/tree/master/libbpf-tools) | Production reference for compat.bpf.h, core_fixes.bpf.h, cgroup filtering (ADR-002-supplement) |
| [Elastic eBPF Profiler](https://github.com/elastic/otel-profiling-agent) | Stack Delta deep unwinding reference implementation (ADR-005) |
