#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Knot E2E validation script (W4 #22).
# Requires: root, BPF-enabled kernel, wperf built with --features bpf.
#
# Compiles the 2-thread mutex workload, records with wperf, and asserts
# that the report contains a Knot with the workload's thread IDs.
#
# Evidence type: manual privileged smoke

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKLOAD_SRC="$SCRIPT_DIR/mutex_knot.c"
WORKLOAD_BIN="$SCRIPT_DIR/mutex_knot"
TRACE_FILE="$(mktemp /tmp/knot_e2e_XXXXXX.wperf)"
REPORT_FILE="$(mktemp /tmp/knot_e2e_XXXXXX.json)"
WPERF="$REPO_DIR/target/release/wperf"
DURATION=3

cleanup() {
    rm -f "$TRACE_FILE" "$REPORT_FILE" "$WORKLOAD_BIN"
}
trap cleanup EXIT

# --- Step 0: Prerequisites ---
echo "=== Knot E2E: checking prerequisites ==="

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: must run as root" >&2
    exit 1
fi

if [ ! -x "$WPERF" ]; then
    echo "Building wperf with BPF support..."
    (cd "$REPO_DIR" && cargo build --release --features bpf)
fi

# --- Step 1: Compile workload ---
echo "=== Knot E2E: compiling mutex_knot ==="
gcc -O2 -pthread -o "$WORKLOAD_BIN" "$WORKLOAD_SRC"

# --- Step 2: Run workload + record ---
echo "=== Knot E2E: recording (${DURATION}s) ==="

# Start workload in background, capture stderr for TIDs
WORKLOAD_LOG="$(mktemp /tmp/knot_e2e_log_XXXXXX.txt)"
"$WORKLOAD_BIN" "$DURATION" 2>"$WORKLOAD_LOG" &
WORKLOAD_PID=$!

# Give workload time to start threads
sleep 0.5

# Record for duration + 1s margin
"$WPERF" record -o "$TRACE_FILE" -d $((DURATION + 1)) 2>&1

# Wait for workload to finish
wait "$WORKLOAD_PID" || true

echo "--- workload log ---"
cat "$WORKLOAD_LOG"

# Extract worker TIDs from workload stderr
TIDS=$(grep -oP 'tid=\K[0-9]+' "$WORKLOAD_LOG" | sort -n)
TID_COUNT=$(echo "$TIDS" | wc -l)
echo "Detected $TID_COUNT worker TIDs: $(echo $TIDS | tr '\n' ' ')"

if [ "$TID_COUNT" -lt 2 ]; then
    echo "ERROR: expected at least 2 worker TIDs, got $TID_COUNT" >&2
    exit 1
fi

TID_ARRAY=($TIDS)
rm -f "$WORKLOAD_LOG"

# --- Step 3: Generate report ---
echo "=== Knot E2E: generating report ==="
"$WPERF" report "$TRACE_FILE" > "$REPORT_FILE"

# --- Step 4: Assertions ---
echo "=== Knot E2E: validating assertions ==="

# Guardrail C: pre-condition checks
EVENTS_READ=$(jq '.stats.events_read' "$REPORT_FILE")
EDGE_COUNT=$(jq '.cascade.graph_metrics.edge_count' "$REPORT_FILE")
INVARIANTS_OK=$(jq '.health.invariants_ok' "$REPORT_FILE")

echo "events_read=$EVENTS_READ, edge_count=$EDGE_COUNT, invariants_ok=$INVARIANTS_OK"

if [ "$EVENTS_READ" -eq 0 ]; then
    echo "FAIL: events_read == 0 — trace is empty (permission issue? probes not attached?)" >&2
    exit 1
fi

if [ "$EDGE_COUNT" -eq 0 ]; then
    echo "FAIL: edge_count == 0 — no wait-for edges correlated" >&2
    exit 1
fi

if [ "$INVARIANTS_OK" != "true" ]; then
    echo "FAIL: invariants_ok != true" >&2
    exit 1
fi

# Core assertion: at least one Knot exists
KNOT_COUNT=$(jq '.knots | length' "$REPORT_FILE")
echo "knot_count=$KNOT_COUNT"

if [ "$KNOT_COUNT" -lt 1 ]; then
    echo "FAIL: no knots detected (expected >= 1)" >&2
    echo "--- full knots output ---"
    jq '.knots' "$REPORT_FILE"
    exit 1
fi

# Guardrail A: verify workload TIDs appear in at least one knot
# Check that at least one knot contains both worker TIDs
TID1="${TID_ARRAY[0]}"
TID2="${TID_ARRAY[1]}"
MATCHING_KNOTS=$(jq --argjson t1 "$TID1" --argjson t2 "$TID2" \
    '[.knots[] | select((.members | index($t1)) and (.members | index($t2)))] | length' \
    "$REPORT_FILE")

echo "knots containing both TIDs ($TID1, $TID2): $MATCHING_KNOTS"

if [ "$MATCHING_KNOTS" -lt 1 ]; then
    echo "FAIL: no knot contains both workload TIDs ($TID1, $TID2)" >&2
    echo "--- all knots ---"
    jq '.knots' "$REPORT_FILE"
    exit 1
fi

# Summary
echo ""
echo "=== Knot E2E: ALL ASSERTIONS PASSED ==="
echo "  events_read:    $EVENTS_READ"
echo "  edge_count:     $EDGE_COUNT"
echo "  invariants_ok:  $INVARIANTS_OK"
echo "  knot_count:     $KNOT_COUNT"
echo "  workload_knots: $MATCHING_KNOTS (both TIDs: $TID1, $TID2)"
