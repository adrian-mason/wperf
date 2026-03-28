#!/usr/bin/env python3
"""
Cascade redistribution oracle — independent Python implementation.

Reads a JSON graph from stdin, runs cascade redistribution, writes
per-edge attributed_delay to stdout as JSON.

Input format:
{
  "nodes": [{"tid": 1, "kind": "UserThread"}, ...],
  "edges": [{"src": 1, "dst": 2, "start_ms": 0, "end_ms": 100}, ...]
}

Output format:
{
  "edges": [{"src": 1, "dst": 2, "raw_wait_ms": 100, "attributed_delay_ms": 20}, ...]
}
"""

import json
import sys

MAX_DEPTH = 10


def sweep_line_partition(outgoing, window_start, window_end):
    """Decompose overlapping edges into non-overlapping elementary intervals."""
    clipped = []
    for dst, e_start, e_end in outgoing:
        # Clip to window
        s = max(e_start, window_start)
        e = min(e_end, window_end)
        if s < e:
            clipped.append((dst, s, e))

    if not clipped:
        return []

    # Collect boundary points
    points = sorted(set(
        p for _, s, e in clipped for p in (s, e)
    ))

    intervals = []
    for i in range(len(points) - 1):
        s, e = points[i], points[i + 1]
        if s >= e:
            continue
        targets = sorted(set(
            dst for dst, cs, ce in clipped if cs <= s and ce >= e
        ))
        if targets:
            intervals.append((s, e, targets))

    return intervals


def count_concurrent_waiters(graph, target, w_start, w_end):
    """Count distinct threads waiting for target during [w_start, w_end)."""
    waiters = set()
    for src, dst, e_start, e_end in graph["edge_list"]:
        if dst == target:
            # Check overlap
            s = max(e_start, w_start)
            e = min(e_end, w_end)
            if s < e:
                waiters.add(src)
    return max(len(waiters), 1)


def compute_cascade(graph, current, w_start, w_end, depth, path):
    """Recursive cascade computation. Returns total propagated weight."""
    if depth >= MAX_DEPTH or current in path:
        return 0

    outgoing = [
        (dst, e_start, e_end)
        for src, dst, e_start, e_end in graph["edge_list"]
        if src == current
    ]

    intervals = sweep_line_partition(outgoing, w_start, w_end)
    if not intervals:
        return 0

    path.add(current)
    total_propagated = 0

    for i_start, i_end, targets in intervals:
        target_count = max(len(targets), 1)
        duration = i_end - i_start

        for next_node in targets:
            compute_cascade(
                graph, next_node, i_start, i_end, depth + 1, path
            )
            child_absorbed = duration
            external = count_concurrent_waiters(
                graph, next_node, i_start, i_end
            )
            transfer = child_absorbed // target_count // max(external, 1)
            total_propagated += transfer

    path.remove(current)
    return total_propagated


def cascade_engine(graph):
    """Run cascade redistribution. Returns list of (src, dst, raw, attributed)."""
    results = []

    for src, dst, e_start, e_end in graph["edge_list"]:
        raw_wait = e_end - e_start
        path = {src}
        propagated = compute_cascade(
            graph, dst, e_start, e_end, 1, path
        )
        attributed = max(0, raw_wait - propagated)
        results.append({
            "src": src,
            "dst": dst,
            "raw_wait_ms": raw_wait,
            "attributed_delay_ms": attributed,
        })

    return results


def main():
    data = json.load(sys.stdin)

    # Build edge list
    graph = {
        "edge_list": [
            (e["src"], e["dst"], e["start_ms"], e["end_ms"])
            for e in data["edges"]
        ]
    }

    results = cascade_engine(graph)
    # Sort for deterministic output
    results.sort(key=lambda e: (e["src"], e["dst"]))

    json.dump({"edges": results}, sys.stdout)


if __name__ == "__main__":
    main()
