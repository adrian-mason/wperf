#!/usr/bin/env python3
"""
Gate 0: Cascade redistribution validation for LINEAR graphs only.

Scope limitation: this script does NOT implement sweep-line partition
for overlapping outgoing edges (BUG-3 in ADR-007). It produces correct
results ONLY for graphs where each node has at most one overlapping
outgoing dependency in any time slice (linear chains, simple non-
overlapping topologies). For graphs with concurrent overlapping targets,
the Rust production implementation (src/graph/sweep.rs) is authoritative.

This script is a validation fixture, not a general-purpose differential
testing oracle. Phase 0 differential testing (Issue #20) should restrict
test inputs to non-overlapping topologies when comparing against this
script, and rely on invariant assertions (I-2 through I-7, ADR-016) for complex graphs.

ADR-009 Amendment 2026-04-25 — synthetic closure-return edges (Disk->User
completion-event edges, and Network/Softirq analogues) are cascade-terminal:
they participate in Tarjan SCC analysis but are NOT traversed by the cascade
redistribution algorithm. The `add_edge(..., kind="synthetic_closure_return")`
marker on Edge tells `_compute_cascade` to skip the edge in BOTH outgoing
traversal AND any waiter-counting logic; applying the filter to only one
site silently halves the cascade transfer for the forward edge (Probe
finding, PR #120 5-way thread). The filter is behaviorally inert in this
oracle's supported topologies (2-cycle synthetic-edge graphs are out of
scope per the linear-chain limitation), but the rule must be encoded here
for forward-compat with future oracle extensions and for documentation
parity with the Rust production cascade.

Validates:
1. SCC/Knot detection using NetworkX
2. Cascade redistribution on linear/tree graphs (ADR-007 pseudocode)
3. Figure 4 expected output: Network=80ms, Parser=20ms, total=100ms
"""

import networkx as nx
import sys


# =============================================================================
# Part 1: SCC / Knot Detection (validates bottleneck.py's core)
# =============================================================================

def find_knots(G):
    """Identify Knots (sink SCCs with out-degree 0 in condensation DAG).
    This replicates bottleneck.py scc_graph() lines 362-392."""
    sccs = [list(c) for c in sorted(
        nx.strongly_connected_components(G), key=len, reverse=True)]

    knots = []
    for i, scc_i in enumerate(sccs):
        outgoing = 0.0
        for j, scc_j in enumerate(sccs):
            if i == j:
                continue
            for m in scc_i:
                for n in scc_j:
                    if G.has_edge(m, n):
                        outgoing += G[m][n]['weight']
        if outgoing == 0:
            knots.append(scc_i)
    return sccs, knots


def test_scc_simple():
    """Test SCC on a simple cycle: A->B->C->A (single SCC, is a Knot)."""
    G = nx.DiGraph()
    G.add_weighted_edges_from([
        ('A', 'B', 10.0),
        ('B', 'C', 20.0),
        ('C', 'A', 5.0),
    ])
    sccs, knots = find_knots(G)
    assert len(sccs) == 1, f"Expected 1 SCC, got {len(sccs)}"
    assert len(knots) == 1, f"Expected 1 Knot, got {len(knots)}"
    assert set(sccs[0]) == {'A', 'B', 'C'}
    print("[PASS] SCC simple cycle: 1 SCC, 1 Knot")


def test_scc_with_sink():
    """Test SCC with a sink node: A->B->A (cycle) + B->C (sink).
    Expected: 2 SCCs, Knot = {C} (trivial sink, but out-degree 0)."""
    G = nx.DiGraph()
    G.add_weighted_edges_from([
        ('A', 'B', 10.0),
        ('B', 'A', 5.0),
        ('B', 'C', 15.0),
    ])
    sccs, knots = find_knots(G)
    assert len(sccs) == 2, f"Expected 2 SCCs, got {len(sccs)}"
    # {C} is a trivial sink SCC (out-degree 0)
    # {A,B} is a non-sink SCC (has edge to C)
    sink_sccs = [s for s in sccs if len(s) == 1 and s[0] == 'C']
    assert len(sink_sccs) == 1, "Expected {C} as trivial sink"
    print("[PASS] SCC with sink: 2 SCCs, sink={C}")


# =============================================================================
# Part 2: Cascade Redistribution (ADR-007 pseudocode implementation)
# =============================================================================

class TimeWindow:
    """A time interval [start, end) in milliseconds."""
    def __init__(self, start, end):
        self.start = start
        self.end = end

    def duration(self):
        return self.end - self.start

    def overlap(self, other):
        """Return the overlapping TimeWindow, or None."""
        s = max(self.start, other.start)
        e = min(self.end, other.end)
        if s < e:
            return TimeWindow(s, e)
        return None

    def __repr__(self):
        return f"[{self.start}, {self.end})"


class Edge:
    def __init__(self, src, dst, window, raw_wait, kind="normal"):
        self.src = src
        self.dst = dst
        self.window = window
        self.raw_wait = raw_wait
        self.attributed = raw_wait  # initially = raw
        # Edge kind — "normal" or "synthetic_closure_return". The latter
        # mirrors EdgeKind::SyntheticClosureReturn in the Rust impl
        # (ADR-009 Amendment 2026-04-25). Cascade-terminal: not traversed
        # in either outgoing-walk or waiter-counting.
        self.kind = kind


class CascadeGraph:
    """Minimal graph for cascade redistribution testing."""

    def __init__(self):
        self.edges = []  # list of Edge
        self.outgoing = {}  # node -> [Edge]

    def add_edge(self, src, dst, start, end, kind="normal"):
        raw_wait = end - start
        e = Edge(src, dst, TimeWindow(start, end), raw_wait, kind=kind)
        self.edges.append(e)
        self.outgoing.setdefault(src, []).append(e)

    def get_outgoing(self, node):
        # ADR-009 Amendment 2026-04-25: synthetic closure-return edges are
        # cascade-terminal — skip them in outgoing traversal. The Rust
        # production cascade applies the same filter to count_concurrent_waiters
        # incoming-edge enumeration; this oracle's linear-chain scope does not
        # exercise count_concurrent_waiters, so only the outgoing-side filter
        # is needed here. Both filters MUST land together in the Rust impl.
        return [e for e in self.outgoing.get(node, []) if e.kind != "synthetic_closure_return"]


def cascade_redistribute(graph, max_depth=10):
    """
    ADR-007 cascade redistribution algorithm.

    For each edge (A->B), traverse B's outgoing edges recursively.
    Weight that B passes downstream is subtracted from B's own blame
    and attributed to the downstream holder.

    Returns dict: node -> attributed_delay_ms
    """
    node_blame = {}  # node -> total attributed blame

    for edge in graph.edges:
        target_window = edge.window
        path = {edge.src}

        propagated, self_blame = _compute_cascade(
            graph, edge.dst, target_window, 1, max_depth, path)

        # edge.src's attributed delay for this edge = what wasn't propagated
        # The dst node absorbs self_blame
        node_blame[edge.dst] = node_blame.get(edge.dst, 0) + self_blame
        # Propagated amount goes to deeper nodes (already accumulated in recursion)

    return node_blame


def _compute_cascade(graph, current, window, depth, max_depth, path):
    """
    Recursive cascade computation per ADR-007 pseudocode.

    Returns (propagated_down, self_blame):
      - propagated_down: weight pushed to nodes deeper than current
      - self_blame: weight retained at current node
    """
    if depth >= max_depth or current in path:
        # Base case: full blame at current node
        return (0, window.duration())

    outgoing = graph.get_outgoing(current)
    if not outgoing:
        # Leaf node: absorbs all blame
        return (0, window.duration())

    # Find overlapping outgoing edges
    overlapping = []
    for e in outgoing:
        ov = window.overlap(e.window)
        if ov:
            overlapping.append((e, ov))

    if not overlapping:
        # No outgoing edges overlap this window: current absorbs all
        return (0, window.duration())

    total_propagated = 0
    coverage_duration = 0

    path.add(current)

    for e, overlap_window in overlapping:
        coverage_duration += overlap_window.duration()
        target_count = len(overlapping)  # concurrent targets

        propagated_down, child_self_blame = _compute_cascade(
            graph, e.dst, overlap_window, depth + 1, max_depth, path)

        # BUG-2 + NEW-BUG-1 fix: include child_self_blame
        child_subtree_absorbed = propagated_down + child_self_blame
        scale = 1.0 / target_count
        transfer = child_subtree_absorbed * scale

        total_propagated += transfer

    path.remove(current)

    self_blame = window.duration() - coverage_duration
    return (total_propagated, self_blame)


def test_figure4():
    """
    OSDI'18 Figure 4 scenario:

    User waits for Parser: [0, 100ms]
    Parser waits for Network: [20, 100ms]

    Expected cascade result:
    - Network attributed: 80ms (the overlap [20,100))
    - Parser attributed: 20ms (the non-overlapping [0,20))
    - Total: 100ms (matches the original User wait window;
      reasonability check on this Figure 4 graph, not a strict invariant
      — I-1 was retired by ADR-016, production sentinel is `invariants_ok`
      = I-2 ∧ I-7).

    Graph:
        User --[0,100]--> Parser --[20,100]--> Network
    """
    g = CascadeGraph()
    g.add_edge('User', 'Parser', 0, 100)
    g.add_edge('Parser', 'Network', 20, 100)

    # Trace the cascade manually:
    # Edge User->Parser, window [0,100):
    #   Recurse into Parser with window [0,100):
    #     Parser has outgoing Parser->Network [20,100)
    #     Overlap of [0,100) and [20,100) = [20,100) = 80ms
    #     Recurse into Network with window [20,100):
    #       Network has no outgoing edges → leaf
    #       Returns (0, 80)  # self_blame = 80ms
    #     child_subtree_absorbed = 0 + 80 = 80
    #     scale = 1/1 = 1.0
    #     transfer = 80 * 1.0 = 80 → propagated to Network
    #     total_propagated = 80
    #     coverage_duration = 80 (the overlap)
    #     self_blame = 100 - 80 = 20 → Parser's own blame
    #   Returns (80, 20)
    # Result: Parser = 20ms, Network = 80ms, Total = 100ms

    blame = cascade_redistribute(g)

    parser_blame = blame.get('Parser', 0)
    network_blame = blame.get('Network', 0)
    total = parser_blame + network_blame

    print(f"  Parser attributed:  {parser_blame:.1f} ms")
    print(f"  Network attributed: {network_blame:.1f} ms")
    print(f"  Total:              {total:.1f} ms")

    assert abs(parser_blame - 20.0) < 0.01, f"Parser should be 20ms, got {parser_blame}"
    assert abs(network_blame - 80.0) < 0.01, f"Network should be 80ms, got {network_blame}"
    assert abs(total - 100.0) < 0.01, f"Total should be 100ms (conservation), got {total}"
    print("[PASS] Figure 4: Parser=20ms, Network=80ms, Total=100ms (conservation holds)")


def test_figure4_extended():
    """
    Extended scenario: 3-node linear chain with partial overlap.

    User --[0,100]--> Parser --[20,100]--> Network --[50,100]--> Disk

    Expected:
    - Disk: 50ms (overlap of [20,100) and [50,100) = [50,100))
    - Network: 80 - 50 = 30ms (overlap [20,100) minus what's pushed to Disk)
    - Parser: 20ms (non-overlapping [0,20))
    - Total: 100ms
    """
    g = CascadeGraph()
    g.add_edge('User', 'Parser', 0, 100)
    g.add_edge('Parser', 'Network', 20, 100)
    g.add_edge('Network', 'Disk', 50, 100)

    blame = cascade_redistribute(g)

    parser_blame = blame.get('Parser', 0)
    network_blame = blame.get('Network', 0)
    disk_blame = blame.get('Disk', 0)
    total = parser_blame + network_blame + disk_blame

    print(f"  Parser attributed:  {parser_blame:.1f} ms")
    print(f"  Network attributed: {network_blame:.1f} ms")
    print(f"  Disk attributed:    {disk_blame:.1f} ms")
    print(f"  Total:              {total:.1f} ms")

    assert abs(total - 100.0) < 0.01, f"Total should be 100ms (conservation), got {total}"
    print(f"[PASS] Extended Figure 4: conservation holds (total={total:.1f}ms)")


# =============================================================================
# Main
# =============================================================================

if __name__ == '__main__':
    print("=" * 60)
    print("Gate 0 #8: Cascade Understanding Validation")
    print("=" * 60)

    print("\n--- Part 1: SCC / Knot Detection ---")
    test_scc_simple()
    test_scc_with_sink()

    print("\n--- Part 2: Cascade Redistribution (ADR-007) ---")
    print("\nFigure 4 (Paper scenario):")
    test_figure4()

    print("\nFigure 4 Extended (3-node chain):")
    test_figure4_extended()

    print("\n" + "=" * 60)
    print("All tests passed.")
    print("=" * 60)
