#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Cross-kernel E2E smoke test (W4 #25, extended for #38 P2b-01 commit-7).
# Requires: root, BPF-enabled kernel, wperf built with --features bpf.
#
# Verifies the full pipeline (record → report) works on the current kernel:
# - BPF skeleton loads and attaches (CO-RE relocations succeed)
# - Feature probing selects correct transport + tracepoint variant
# - Events captured from scheduler tracepoints
# - Report generation produces valid output with health invariants
# - block_rq tracepoints observable through synthetic User↔Disk edges and
#   the `attributed_delay_ratio` / io_* HealthMetrics fields (commit-7)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKLOAD_SRC="$SCRIPT_DIR/mutex_knot.c"
WORKLOAD_BIN="$SCRIPT_DIR/mutex_knot"
TRACE_FILE="$(mktemp /tmp/cross_kernel_e2e_XXXXXX.wperf)"
REPORT_FILE="$(mktemp /tmp/cross_kernel_e2e_XXXXXX.json)"
IO_TRACE_FILE="$(mktemp /tmp/cross_kernel_io_XXXXXX.wperf)"
IO_REPORT_FILE="$(mktemp /tmp/cross_kernel_io_XXXXXX.json)"
IO_WORKLOAD_FILE="$(mktemp /tmp/cross_kernel_io_data_XXXXXX.bin)"
WPERF="$REPO_DIR/target/release/wperf"
DURATION=3

cleanup() {
    [ -n "${WORKLOAD_PID:-}" ] && kill "$WORKLOAD_PID" 2>/dev/null || true
    [ -n "${IO_WORKLOAD_PID:-}" ] && kill "$IO_WORKLOAD_PID" 2>/dev/null || true
    rm -f "$TRACE_FILE" "$REPORT_FILE" "$WORKLOAD_BIN" "${RECORD_STDERR:-}" \
          "$IO_TRACE_FILE" "$IO_REPORT_FILE" "$IO_WORKLOAD_FILE" \
          "${IO_RECORD_STDERR:-}"
}
trap cleanup EXIT

# --- Environment info ---
echo "=== Cross-Kernel E2E: environment ==="
echo "  kernel:  $(uname -r)"
echo "  arch:    $(uname -m)"
echo "  distro:  $(cat /etc/os-release 2>/dev/null | grep PRETTY_NAME | cut -d= -f2 | tr -d '\"' || echo unknown)"
echo "  btf:     $(test -f /sys/kernel/btf/vmlinux && echo 'available' || echo 'unavailable')"

# --- Prerequisites ---
if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: must run as root" >&2
    exit 1
fi

for cmd in gcc jq; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "ERROR: $cmd not found" >&2
        exit 1
    fi
done

if ! test -f /sys/kernel/btf/vmlinux; then
    echo "ERROR: /sys/kernel/btf/vmlinux not found — kernel lacks BTF support" >&2
    echo "CO-RE relocations require BTF. Minimum kernel: 5.2 with CONFIG_DEBUG_INFO_BTF=y" >&2
    exit 1
fi

if [ ! -x "$WPERF" ]; then
    echo "Building wperf with BPF support..."
    (cd "$REPO_DIR" && cargo build --release --features bpf)
fi

# --- Step 1: BPF capability check ---
echo "=== Cross-Kernel E2E: BPF capability check ==="
if ! bpftool prog list >/dev/null 2>&1; then
    echo "WARNING: bpftool prog list failed — BPF syscall may be restricted"
fi

# --- Step 2: Compile workload ---
echo "=== Cross-Kernel E2E: compiling workload ==="
gcc -O2 -pthread -o "$WORKLOAD_BIN" "$WORKLOAD_SRC"

# --- Step 3: Record ---
echo "=== Cross-Kernel E2E: recording (${DURATION}s) ==="

"$WORKLOAD_BIN" "$DURATION" &
WORKLOAD_PID=$!
sleep 0.5

RECORD_STDERR="$(mktemp /tmp/cross_kernel_record_XXXXXX.txt)"
"$WPERF" record -o "$TRACE_FILE" -d $((DURATION + 1)) 2>"$RECORD_STDERR" || {
    echo "FAIL: wperf record exited with error" >&2
    cat "$RECORD_STDERR" >&2
    rm -f "$RECORD_STDERR"
    exit 1
}

wait "$WORKLOAD_PID" || true

echo "--- wperf record stderr ---"
cat "$RECORD_STDERR"
rm -f "$RECORD_STDERR"

# --- Step 4: Report ---
echo "=== Cross-Kernel E2E: generating report ==="
"$WPERF" report "$TRACE_FILE" > "$REPORT_FILE"

# --- Step 5: Assertions ---
echo "=== Cross-Kernel E2E: validating ==="

EVENTS_READ=$(jq '.stats.events_read' "$REPORT_FILE")
EDGE_COUNT=$(jq '.cascade.graph_metrics.edge_count' "$REPORT_FILE")
INVARIANTS_OK=$(jq '.health.invariants_ok' "$REPORT_FILE")
DROP_COUNT=$(jq '.health.drop_count // 0' "$REPORT_FILE")

echo "  events_read:    $EVENTS_READ"
echo "  edge_count:     $EDGE_COUNT"
echo "  invariants_ok:  $INVARIANTS_OK"
echo "  drop_count:     $DROP_COUNT"

if [ "$EVENTS_READ" -eq 0 ]; then
    echo "FAIL: events_read == 0 — no events captured" >&2
    exit 1
fi

if [ "$EDGE_COUNT" -eq 0 ]; then
    echo "FAIL: edge_count == 0 — no wait-for edges" >&2
    exit 1
fi

if [ "$INVARIANTS_OK" != "true" ]; then
    echo "FAIL: health invariants violated" >&2
    jq '.health' "$REPORT_FILE" >&2
    exit 1
fi

TRACE_SIZE=$(stat -c%s "$TRACE_FILE" 2>/dev/null || stat -f%z "$TRACE_FILE")
echo "  trace_size:     $TRACE_SIZE bytes"

echo ""
echo "=== Cross-Kernel E2E Phase 1 (mutex_knot): PASSED ==="
echo "  kernel $(uname -r): record + report pipeline verified"
echo "  events=$EVENTS_READ edges=$EDGE_COUNT invariants=ok drops=$DROP_COUNT"

# =============================================================================
# Phase 2 — block_rq runtime smoke (#38 P2b-01 commit-7)
# =============================================================================
# Exercises block_rq_issue / block_rq_complete end-to-end:
#   1. wperf record runs during a `dd` pass that forces synchronous disk I/O
#      via O_SYNC + conv=fsync so block_rq_issue/complete fire observably.
#   2. wperf report produces the health fields introduced in commits 3-5:
#      io_orphan_complete_count, io_pending_at_end_count,
#      io_userspace_pair_collision_count, attributed_delay_ratio.
#   3. Assertions: the IO health fields are non-null (tracing actually ran),
#      attributed_delay_ratio is in [0.0, 1.0] (well-defined), and — if the
#      ratio is a number — the graph contains at least one IoBlock-origin
#      DISK_TID(-5) node or edge indicating real synthetic-edge generation.
#
# NOTE: on an idle GitHub runner a dd burst may still miss block_rq_issue on
# tmpfs or very short-duration runs. We request a small but non-trivial
# workload (4MB forced-fsync) and keep the assertions loose enough that the
# test passes on kernels where block_rq didn't fire (fields are None rather
# than populated) — that path is still a correctness signal.

echo ""
echo "=== Cross-Kernel E2E Phase 2: block_rq smoke ==="

# Run wperf record during a sync-fsync dd pass. We write to a file on the
# default tmpdir filesystem; on most GitHub runners this is a backing
# filesystem that does hit block_rq tracepoints (not pure tmpfs).
IO_RECORD_STDERR="$(mktemp /tmp/cross_kernel_io_record_XXXXXX.txt)"

# Background the dd workload and sample during wperf recording. 4MB fsynced
# in 4KB writes yields ~1024 block_rq_issue events on backing devices.
(
    sleep 0.2
    dd if=/dev/zero of="$IO_WORKLOAD_FILE" bs=4K count=1024 conv=fsync 2>/dev/null || true
    sync
) &
IO_WORKLOAD_PID=$!

"$WPERF" record -o "$IO_TRACE_FILE" -d $((DURATION + 1)) 2>"$IO_RECORD_STDERR" || {
    echo "FAIL: wperf record (IO phase) exited with error" >&2
    cat "$IO_RECORD_STDERR" >&2
    rm -f "$IO_RECORD_STDERR"
    exit 1
}
wait "$IO_WORKLOAD_PID" || true

echo "--- wperf record (IO phase) stderr ---"
cat "$IO_RECORD_STDERR"
rm -f "$IO_RECORD_STDERR"

"$WPERF" report "$IO_TRACE_FILE" > "$IO_REPORT_FILE"

IO_EVENTS=$(jq '.stats.events_read' "$IO_REPORT_FILE")
IO_INVARIANTS=$(jq '.health.invariants_ok' "$IO_REPORT_FILE")
IO_ORPHAN=$(jq '.health.io_orphan_complete_count' "$IO_REPORT_FILE")
IO_PENDING=$(jq '.health.io_pending_at_end_count' "$IO_REPORT_FILE")
IO_COLLISION=$(jq '.health.io_userspace_pair_collision_count' "$IO_REPORT_FILE")
# attributed_delay_ratio is now a per-IO-pseudo-thread map (post-commit-10
# per-P promotion per spec §7.3 (a) per-P quantification). JSON shape:
#   null  — (b) hard precondition: no IO pseudo-thread has a defined ratio
#   {"disk": <number>, "nic": <number>, ...}  — per-P entries in [0.0, 1.0]
IO_RATIO_RAW=$(jq -c '.health.attributed_delay_ratio' "$IO_REPORT_FILE")
IO_EDGE_COUNT=$(jq '.cascade.graph_metrics.edge_count' "$IO_REPORT_FILE")

echo "  events_read:                    $IO_EVENTS"
echo "  edge_count:                     $IO_EDGE_COUNT"
echo "  invariants_ok:                  $IO_INVARIANTS"
echo "  io_orphan_complete_count:       $IO_ORPHAN"
echo "  io_pending_at_end_count:        $IO_PENDING"
echo "  io_userspace_pair_collision:    $IO_COLLISION"
echo "  attributed_delay_ratio:         $IO_RATIO_RAW"

if [ "$IO_INVARIANTS" != "true" ]; then
    echo "FAIL: IO-phase invariants violated" >&2
    jq '.health' "$IO_REPORT_FILE" >&2
    exit 1
fi

# IO health fields must always be present (Some(_)) — confirms the plumbing
# is wired through build_report. Null values would mean the commit-5
# wiring regressed.
if [ "$IO_ORPHAN" = "null" ] || [ "$IO_PENDING" = "null" ] || [ "$IO_COLLISION" = "null" ]; then
    echo "FAIL: HealthMetrics IO counter fields must be populated (commit-5 wiring)" >&2
    jq '.health' "$IO_REPORT_FILE" >&2
    exit 1
fi

# Per-P validation: each value must be a number in [0.0, 1.0].
if [ "$IO_RATIO_RAW" != "null" ]; then
    RATIO_OK=$(jq -r '.health.attributed_delay_ratio | to_entries | all(.value | type == "number" and . >= 0.0 and . <= 1.0) | if . then "ok" else "bad" end' "$IO_REPORT_FILE")
    if [ "$RATIO_OK" != "ok" ]; then
        echo "FAIL: per-P attributed_delay_ratio entries out of [0.0, 1.0]: $IO_RATIO_RAW" >&2
        exit 1
    fi
fi

# If attributed_delay_ratio is populated, block_rq actually fired and
# DISK_TID(-5) should appear as src or dst of at least one edge.
if [ "$IO_RATIO_RAW" != "null" ]; then
    DISK_EDGES=$(jq '[.cascade.edges[] | select(.src == -5 or .dst == -5)] | length' "$IO_REPORT_FILE")
    echo "  disk_pseudo_edges:              $DISK_EDGES"
    if [ "$DISK_EDGES" -eq 0 ]; then
        echo "FAIL: ratio populated but no DISK_TID(-5) edges present — synthetic edge injection inconsistent" >&2
        exit 1
    fi
fi

echo ""
echo "=== Cross-Kernel E2E Phase 2 (block_rq smoke): PASSED ==="
echo "  kernel $(uname -r): block_rq path exercised"
echo "  events=$IO_EVENTS edges=$IO_EDGE_COUNT ratio=$IO_RATIO_RAW"
echo ""
echo "=== Cross-Kernel E2E: ALL PHASES PASSED ==="
