#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Cross-kernel E2E smoke test (W4 #25).
# Requires: root, BPF-enabled kernel, wperf built with --features bpf.
#
# Verifies the full pipeline (record → report) works on the current kernel:
# - BPF skeleton loads and attaches (CO-RE relocations succeed)
# - Feature probing selects correct transport + tracepoint variant
# - Events captured from scheduler tracepoints
# - Report generation produces valid output with health invariants

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKLOAD_SRC="$SCRIPT_DIR/mutex_knot.c"
WORKLOAD_BIN="$SCRIPT_DIR/mutex_knot"
TRACE_FILE="$(mktemp /tmp/cross_kernel_e2e_XXXXXX.wperf)"
REPORT_FILE="$(mktemp /tmp/cross_kernel_e2e_XXXXXX.json)"
WPERF="$REPO_DIR/target/release/wperf"
DURATION=3

cleanup() {
    rm -f "$TRACE_FILE" "$REPORT_FILE" "$WORKLOAD_BIN"
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
DROP_COUNT=$(jq '.stats.drop_count // 0' "$REPORT_FILE")

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
echo "=== Cross-Kernel E2E: PASSED ==="
echo "  kernel $(uname -r): record + report pipeline verified"
echo "  events=$EVENTS_READ edges=$EDGE_COUNT invariants=ok drops=$DROP_COUNT"
