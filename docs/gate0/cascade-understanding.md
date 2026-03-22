# Gate 0 — Cascade Algorithm Understanding

- **Issue:** #8
- **Date:** 2026-03-22
- **Pass criteria:** Figure 4 output matches paper exactly (Network=80ms, Parser=20ms, total=100ms)

## 1. Critical Finding from #54

**bottleneck.py does NOT implement cascade redistribution.** It is an interactive SCC visualization tool that reads pre-aggregated edge weights from Analyzer.java's `waitfor` file. The cascade redistribution algorithm described in the OSDI'18 paper has no complete reference implementation in wPerf-origin.

Consequently, this issue validates:
- **SCC/Knot detection** — using NetworkX (same as bottleneck.py)
- **Cascade redistribution** — independently implemented from ADR-007 pseudocode
- **Figure 4 paper scenario** — exact numeric match

## 2. Cascade Redistribution Algorithm

### 2.1 Core Concept

The cascade algorithm answers: "When thread A waits for thread B, and B is itself waiting for C, how much of A's wait should be blamed on B vs C?"

**Naive approach:** Blame B for the full wait. This produces false bottlenecks — B is also a victim.

**Cascade approach:** Recursively traverse B's outgoing edges. If B was waiting for C during the overlap period, push that weight to C. B retains blame only for time it was the root cause.

### 2.2 Algorithm (ADR-007 Pseudocode)

```
cascade_engine(graph, max_depth=10):
    for each edge (A → B) with time_window W:
        path = {A}
        (propagated, self_blame) = compute_cascade(graph, B, W, depth=1, max_depth, path)
        B.attributed += self_blame
        # propagated amount already distributed to deeper nodes

compute_cascade(graph, current, window, depth, max_depth, path):
    if depth >= max_depth or current in path:
        return (0, window.duration)  // base case: full blame here

    overlapping = [e for e in current.outgoing if e.window overlaps window]
    if empty(overlapping):
        return (0, window.duration)  // leaf: absorbs all

    path.add(current)  // BUG-1 FIX: outside loop
    total_propagated = 0
    coverage = 0

    for (edge, overlap) in overlapping:
        coverage += overlap.duration
        (prop_down, child_blame) = compute_cascade(graph, edge.dst, overlap, depth+1, ...)
        child_absorbed = prop_down + child_blame  // NEW-BUG-1 FIX: include child_blame
        scale = 1.0 / len(overlapping)
        total_propagated += child_absorbed * scale

    path.remove(current)  // BUG-1 FIX: outside loop
    self_blame = window.duration - coverage
    return (total_propagated, self_blame)
```

### 2.3 Key Properties

| Property | Detail |
|----------|--------|
| **Weight conservation** | Σ attributed = Σ raw_wait (I-1 invariant) |
| **Proportional allocation** | When current node waits for N targets simultaneously, each gets 1/N |
| **Cycle handling** | `path` set prevents infinite recursion; cycle node absorbs remaining blame |
| **Depth limit** | `max_depth=10` prevents deep recursion; truncated branches absorb remaining weight |
| **Sweep-line partition** | Overlapping outgoing edges are decomposed into elementary intervals to avoid double-counting (BUG-3 fix) |

## 3. Figure 4 Validation

### 3.1 Input Graph

```mermaid
graph LR
    User -->|"[0, 100ms]"| Parser
    Parser -->|"[20, 100ms]"| Network
```

### 3.2 Manual Cascade Trace

**Edge: User → Parser, window [0, 100ms):**

1. Enter `compute_cascade(Parser, [0,100), depth=1)`
2. Parser has outgoing edge Parser→Network with window [20, 100ms)
3. Overlap of [0,100) ∩ [20,100) = **[20, 100) = 80ms**
4. Recurse: `compute_cascade(Network, [20,100), depth=2)`
   - Network has no outgoing edges → **leaf node**
   - Returns `(propagated=0, self_blame=80)`
5. Back in Parser:
   - `child_absorbed = 0 + 80 = 80`
   - `scale = 1/1 = 1.0` (only 1 overlapping target)
   - `total_propagated = 80`
   - `coverage = 80` (the overlap duration)
   - `self_blame = 100 - 80 = 20` (Parser's own blame)
6. Returns `(propagated=80, self_blame=20)`

**Result:**
- Parser attributed: **20 ms** (the [0, 20ms) window where Parser alone was the holder)
- Network attributed: **80 ms** (the [20, 100ms) overlap where Network was the root cause)
- Total: **100 ms** (conservation: 20 + 80 = 100 = raw_wait)

### 3.3 Extended Scenario: 3-Node Chain

```mermaid
graph LR
    User -->|"[0, 100ms]"| Parser
    Parser -->|"[20, 100ms]"| Network
    Network -->|"[50, 100ms]"| Disk
```

**Cascade trace:**
- Disk (leaf): absorbs 50ms from overlap [50,100)
- Network: overlap with Disk [50,100) = 50ms pushed down; self_blame = 80-50 = 30ms
- Parser: overlap with Network [20,100) = 80ms, of which 50ms propagated to Disk; self_blame = 100-80 = 20ms

**Result:** Parser=20ms, Network=30ms, Disk=50ms, Total=100ms (**conservation holds**)

## 4. Test Results

```
============================================================
Gate 0 #8: Cascade Understanding Validation
============================================================

--- Part 1: SCC / Knot Detection ---
[PASS] SCC simple cycle: 1 SCC, 1 Knot
[PASS] SCC with sink: 2 SCCs, sink={C}

--- Part 2: Cascade Redistribution (ADR-007) ---

Figure 4 (Paper scenario):
  Parser attributed:  20.0 ms
  Network attributed: 80.0 ms
  Total:              100.0 ms
[PASS] Figure 4: Parser=20ms, Network=80ms, Total=100ms (conservation holds)

Figure 4 Extended (3-node chain):
  Parser attributed:  20.0 ms
  Network attributed: 30.0 ms
  Disk attributed:    50.0 ms
  Total:              100.0 ms
[PASS] Extended Figure 4: conservation holds (total=100.0ms)

============================================================
All tests passed.
============================================================
```

## 5. Discoveries and Edge Cases

### 5.1 bottleneck.py's `waitfor` Input Format

The `waitfor` file uses **space-separated** `<from> <to> <weight>` format (not CSV despite `waitfor.csv` sample file having comma-separated headers). The sample `waitfor.csv` in the repo appears to be an export artifact, not the actual inter-process format.

### 5.2 Thread Grouping Before SCC

bottleneck.py aggregates individual thread edges into **thread-group edges** before SCC analysis (lines 289-314). This means the SCC operates on groups, not individual threads. wPerf's Phase 0 should decide whether to group before or after cascade.

### 5.3 Edge Filtering Threshold

bottleneck.py filters edges below `totaltime * removeThreshold` (line 321-325). This is analogous to wPerf's §3.1 Step 2 (Noise Edge Pruning) but uses a relative threshold rather than the absolute 50μs spurious wakeup filter.

### 5.4 Pseudo-Thread Exclusion

bottleneck.py explicitly excludes several pseudo-thread types from the graph (line 268):
```python
if (nodeB==-99) or (nodeB==0) or (nodeB==-16) or (nodeB==-15) or (nodeB == -2) or (nodeA in softirq):
    continue
```

This means softirq (-16), hardirq (-15), timer (-2), unknown (-99), and scheduler (0) edges are **dropped before SCC**. Only NIC (-4) and Disk (-5) pseudo-threads participate. wPerf's design (ADR-009) explicitly includes all pseudo-threads in the WFG — a divergence from the original implementation worth noting.

## 6. Impact on Phase 0

1. **Cascade has no oracle.** Differential testing (Issue #20) cannot compare against bottleneck.py. The ADR-007 pseudocode and manual traces (like Figure 4 above) are the only validation sources.

2. **Weight conservation (I-1) is the critical safety net.** Since there's no reference to diff against, the invariant `Σ attributed == Σ raw_wait` is the strongest proof of correctness.

3. **The Python test harness (`run_figure4.py`) can serve as a minimal oracle.** It implements the cascade from ADR-007 pseudocode in ~80 lines of Python. Phase 0's differential testing could compare Rust output against this script for simple scenarios.
