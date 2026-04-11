#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# W4 #23 overhead baseline validation script.
# Requires: root, BPF-enabled kernel, wperf built with --features bpf,
#           stress-ng, pidstat (sysstat package).
#
# Authoritative Input: final-design.md §6.7
#   Workload:    stress-ng --matrix 64 (or equivalent, >100K sched_switch/sec)
#   Measurement: pidstat -p <pid> 1, 60-second window
#   Threshold:   <3% single-core equivalent CPU
#
# Evidence type: manual privileged smoke (Directive 5)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WPERF="$REPO_DIR/target/release/wperf"
DURATION=60
NUM_RUNS=3
CPU_THRESHOLD=3.0
MIN_EVENTS_PER_SEC=100000

cleanup() {
    [ -n "${STRESS_PID:-}" ] && kill "$STRESS_PID" 2>/dev/null || true
    [ -n "${WPERF_PID:-}" ] && kill "$WPERF_PID" 2>/dev/null || true
    [ -n "${PIDSTAT_PID:-}" ] && kill "$PIDSTAT_PID" 2>/dev/null || true
    rm -f "${TRACE_FILE:-}" "${PIDSTAT_LOG:-}"
}
trap cleanup EXIT

# --- Step 0: Prerequisites ---
echo "=== Overhead Baseline: checking prerequisites ==="

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: must run as root" >&2
    exit 1
fi

if ! command -v pidstat &>/dev/null; then
    echo "ERROR: pidstat not found — install sysstat package" >&2
    exit 1
fi

if ! command -v stress-ng &>/dev/null; then
    echo "ERROR: stress-ng not found" >&2
    exit 1
fi

if ! command -v jq &>/dev/null; then
    echo "ERROR: jq not found" >&2
    exit 1
fi

if [ ! -x "$WPERF" ]; then
    echo "Building wperf with BPF support..."
    (cd "$REPO_DIR" && cargo build --release --features bpf)
fi

# --- Step 1: Determine workload stressor ---
echo "=== Overhead Baseline: calibrating stressor ==="

STRESSOR="--matrix 64"
STRESSOR_NAME="matrix-64"

CALIBRATE_TRACE="$(mktemp /tmp/overhead_calibrate_XXXXXX.wperf)"
stress-ng $STRESSOR --timeout 5 &>/dev/null &
CAL_STRESS_PID=$!
sleep 0.5

"$WPERF" record -o "$CALIBRATE_TRACE" -d 5 2>&1 &
CAL_WPERF_PID=$!
wait "$CAL_WPERF_PID" 2>/dev/null || true
wait "$CAL_STRESS_PID" 2>/dev/null || true

CAL_REPORT="$("$WPERF" report "$CALIBRATE_TRACE" 2>/dev/null)"
CAL_EVENTS=$(echo "$CAL_REPORT" | jq '.stats.events_read')
CAL_RATE=$((CAL_EVENTS / 5))
rm -f "$CALIBRATE_TRACE"

echo "Calibration: $STRESSOR_NAME → $CAL_EVENTS events in 5s ($CAL_RATE events/sec)"

if [ "$CAL_RATE" -lt "$MIN_EVENTS_PER_SEC" ]; then
    echo "WARNING: $STRESSOR_NAME produces $CAL_RATE events/sec (< $MIN_EVENTS_PER_SEC required)"
    echo "Falling back to --context 64 per spec 'or equivalent' clause"
    STRESSOR="--context 64"
    STRESSOR_NAME="context-64"

    CALIBRATE_TRACE="$(mktemp /tmp/overhead_calibrate_XXXXXX.wperf)"
    stress-ng $STRESSOR --timeout 5 &>/dev/null &
    CAL_STRESS_PID=$!
    sleep 0.5

    "$WPERF" record -o "$CALIBRATE_TRACE" -d 5 2>&1 &
    CAL_WPERF_PID=$!
    wait "$CAL_WPERF_PID" 2>/dev/null || true
    wait "$CAL_STRESS_PID" 2>/dev/null || true

    CAL_REPORT="$("$WPERF" report "$CALIBRATE_TRACE" 2>/dev/null)"
    CAL_EVENTS=$(echo "$CAL_REPORT" | jq '.stats.events_read')
    CAL_RATE=$((CAL_EVENTS / 5))
    rm -f "$CALIBRATE_TRACE"

    echo "Calibration: $STRESSOR_NAME → $CAL_EVENTS events in 5s ($CAL_RATE events/sec)"

    if [ "$CAL_RATE" -lt "$MIN_EVENTS_PER_SEC" ]; then
        echo "ERROR: neither stressor reaches $MIN_EVENTS_PER_SEC events/sec" >&2
        echo "  This may indicate insufficient CPU cores or a low-activity kernel config" >&2
        exit 1
    fi
fi

echo "Using stressor: $STRESSOR_NAME ($CAL_RATE events/sec)"

# --- Step 2: Run N measurement iterations ---
echo ""
echo "=== Overhead Baseline: running $NUM_RUNS iterations (${DURATION}s each) ==="

declare -a RUN_CPU_MEANS
declare -a RUN_EVENT_COUNTS
declare -a RUN_DROP_COUNTS

for RUN in $(seq 1 "$NUM_RUNS"); do
    echo ""
    echo "--- Run $RUN/$NUM_RUNS ---"

    TRACE_FILE="$(mktemp /tmp/overhead_run_XXXXXX.wperf)"
    PIDSTAT_LOG="$(mktemp /tmp/overhead_pidstat_XXXXXX.log)"

    # Start stress-ng background load
    stress-ng $STRESSOR --timeout $((DURATION + 5)) &>/dev/null &
    STRESS_PID=$!
    sleep 1

    # Start wperf record
    "$WPERF" record -o "$TRACE_FILE" -d "$DURATION" 2>&1 &
    WPERF_PID=$!
    sleep 0.5

    # Start pidstat monitoring of wperf process
    pidstat -p "$WPERF_PID" 1 "$DURATION" > "$PIDSTAT_LOG" 2>&1 &
    PIDSTAT_PID=$!

    # Wait for wperf to finish
    wait "$WPERF_PID" 2>/dev/null || true
    WPERF_PID=""

    # Give pidstat a moment to flush, then stop
    sleep 2
    kill "$PIDSTAT_PID" 2>/dev/null || true
    wait "$PIDSTAT_PID" 2>/dev/null || true
    PIDSTAT_PID=""

    # Stop stress-ng
    kill "$STRESS_PID" 2>/dev/null || true
    wait "$STRESS_PID" 2>/dev/null || true
    STRESS_PID=""

    # Parse pidstat output — extract %CPU column (user+sys), skip first/last
    # samples to exclude startup/teardown transients (Challenger review condition).
    # pidstat format: Time  UID  PID  %usr  %system  %guest  %wait  %CPU  CPU  Command
    CPU_VALUES_ALL=$(grep -E '^\s*[0-9]' "$PIDSTAT_LOG" | grep -v 'Average' | awk '{print $8}' | grep -E '^[0-9]')
    CPU_VALUES=$(echo "$CPU_VALUES_ALL" | tail -n +2 | head -n -1)
    if [ -z "$CPU_VALUES" ]; then
        echo "WARNING: no pidstat data for run $RUN — pidstat output:"
        cat "$PIDSTAT_LOG"
        RUN_CPU_MEANS+=("NaN")
    else
        SAMPLE_COUNT=$(echo "$CPU_VALUES" | wc -l)
        CPU_SUM=$(echo "$CPU_VALUES" | awk '{s+=$1} END {print s}')
        CPU_MEAN=$(echo "$CPU_SUM $SAMPLE_COUNT" | awk '{printf "%.2f", $1/$2}')
        echo "  pidstat: $SAMPLE_COUNT samples, mean %CPU = $CPU_MEAN"
        RUN_CPU_MEANS+=("$CPU_MEAN")
    fi

    # Get event count and drop count from wperf report
    REPORT="$("$WPERF" report "$TRACE_FILE" 2>/dev/null)"
    EVENT_COUNT=$(echo "$REPORT" | jq '.stats.events_read')
    DROP_COUNT=$(echo "$REPORT" | jq '.stats.drop_count')
    EVENT_RATE=$((EVENT_COUNT / DURATION))

    echo "  events=$EVENT_COUNT ($EVENT_RATE/sec), drops=$DROP_COUNT"
    RUN_EVENT_COUNTS+=("$EVENT_COUNT")
    RUN_DROP_COUNTS+=("$DROP_COUNT")

    rm -f "$TRACE_FILE" "$PIDSTAT_LOG"
    TRACE_FILE=""
    PIDSTAT_LOG=""
done

# --- Step 3: Compute aggregate statistics ---
echo ""
echo "=== Overhead Baseline: computing results ==="

OVERALL_CPU_MEAN=$(printf '%s\n' "${RUN_CPU_MEANS[@]}" | awk '{s+=$1; n++} END {printf "%.2f", s/n}')
OVERALL_CPU_STDDEV=$(printf '%s\n' "${RUN_CPU_MEANS[@]}" | awk -v mean="$OVERALL_CPU_MEAN" '{d=$1-mean; ss+=d*d; n++} END {printf "%.2f", sqrt(ss/n)}')

TOTAL_EVENTS=0
TOTAL_DROPS=0
for i in $(seq 0 $((NUM_RUNS - 1))); do
    TOTAL_EVENTS=$((TOTAL_EVENTS + ${RUN_EVENT_COUNTS[$i]}))
    TOTAL_DROPS=$((TOTAL_DROPS + ${RUN_DROP_COUNTS[$i]}))
done

# --- Step 4: Generate JSON artifact ---
JSON_ARTIFACT="$(mktemp /tmp/overhead_result_XXXXXX.json)"

RUNS_JSON="["
for i in $(seq 0 $((NUM_RUNS - 1))); do
    [ "$i" -gt 0 ] && RUNS_JSON+=","
    RUNS_JSON+="{\"run\":$((i+1)),\"cpu_percent\":${RUN_CPU_MEANS[$i]},\"events\":${RUN_EVENT_COUNTS[$i]},\"drops\":${RUN_DROP_COUNTS[$i]}}"
done
RUNS_JSON+="]"

cat > "$JSON_ARTIFACT" <<ENDJSON
{
  "gate": "W4 #23",
  "spec": "final-design.md §6.7",
  "threshold_cpu_percent": $CPU_THRESHOLD,
  "stressor": "$STRESSOR_NAME",
  "calibration_events_per_sec": $CAL_RATE,
  "duration_seconds": $DURATION,
  "num_runs": $NUM_RUNS,
  "results": {
    "mean_cpu_percent": $OVERALL_CPU_MEAN,
    "stddev_cpu_percent": $OVERALL_CPU_STDDEV,
    "total_events": $TOTAL_EVENTS,
    "total_drops": $TOTAL_DROPS
  },
  "runs": $RUNS_JSON,
  "pass": $(echo "$OVERALL_CPU_MEAN $CPU_THRESHOLD" | awk '{print ($1 < $2) ? "true" : "false"}')
}
ENDJSON

echo "JSON artifact: $JSON_ARTIFACT"
cat "$JSON_ARTIFACT"

# --- Step 5: Assertions ---
echo ""
echo "=== Overhead Baseline: assertions ==="

PASS=$(echo "$OVERALL_CPU_MEAN $CPU_THRESHOLD" | awk '{print ($1 < $2) ? "true" : "false"}')

echo "  stressor:       $STRESSOR_NAME"
echo "  duration:       ${DURATION}s × $NUM_RUNS runs"
echo "  mean %CPU:      $OVERALL_CPU_MEAN (stddev: $OVERALL_CPU_STDDEV)"
echo "  threshold:      < $CPU_THRESHOLD%"
echo "  total events:   $TOTAL_EVENTS"
echo "  total drops:    $TOTAL_DROPS"
echo "  pass:           $PASS"

if [ "$PASS" != "true" ]; then
    echo ""
    echo "FAIL: mean CPU $OVERALL_CPU_MEAN% >= threshold $CPU_THRESHOLD%" >&2
    exit 1
fi

echo ""
echo "=== Overhead Baseline: ALL ASSERTIONS PASSED ==="
