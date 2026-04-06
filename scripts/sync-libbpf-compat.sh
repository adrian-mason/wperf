#!/usr/bin/env bash
# sync-libbpf-compat.sh — Sync compat.bpf.h + core_fixes.bpf.h from bcc/libbpf-tools
#
# This script documents the provenance of vendored BPF headers:
#   - src/bpf/compat.bpf.h  (reserve_buf/submit_buf transport abstraction)
#   - src/bpf/core_fixes.bpf.h (CO-RE field rename fixes, e.g. state/__state)
#
# Fetches directly from iovisor/bcc GitHub repo (no local clone needed).
# Vendored files are NOT verbatim copies — they are adapted for wPerf.
# When updating, diff the upstream changes and manually apply relevant fixes.
#
# Usage: ./scripts/sync-libbpf-compat.sh [--apply]
#   Without --apply: shows diffs only (dry run)
#   With --apply: downloads upstream files to a temp dir for manual diffing

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

GITHUB_RAW="https://raw.githubusercontent.com/iovisor/bcc/master/libbpf-tools"
GITHUB_LOG="https://api.github.com/repos/iovisor/bcc/commits"

VENDORED_COMPAT="$REPO_ROOT/src/bpf/compat.bpf.h"
VENDORED_CORE="$REPO_ROOT/src/bpf/core_fixes.bpf.h"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# Fetch upstream files
echo "=== Fetching upstream files from iovisor/bcc ==="

curl -sfL "$GITHUB_RAW/compat.bpf.h" -o "$TMPDIR/compat.bpf.h" || {
    echo "Error: failed to fetch compat.bpf.h from GitHub"
    exit 1
}
echo "  Downloaded compat.bpf.h"

curl -sfL "$GITHUB_RAW/core_fixes.bpf.h" -o "$TMPDIR/core_fixes.bpf.h" || {
    echo "Error: failed to fetch core_fixes.bpf.h from GitHub"
    exit 1
}
echo "  Downloaded core_fixes.bpf.h"
echo ""

# Show upstream provenance via GitHub API
echo "=== Upstream provenance ==="
echo "compat.bpf.h:"
curl -sf "$GITHUB_LOG?path=libbpf-tools/compat.bpf.h&per_page=3" | \
    python3 -c "
import sys, json
for c in json.load(sys.stdin):
    sha = c['sha'][:12]
    msg = c['commit']['message'].split('\n')[0][:72]
    print(f'  {sha} {msg}')
" 2>/dev/null || echo "  (could not fetch commit history — check network/rate limit)"
echo ""

echo "core_fixes.bpf.h:"
curl -sf "$GITHUB_LOG?path=libbpf-tools/core_fixes.bpf.h&per_page=3" | \
    python3 -c "
import sys, json
for c in json.load(sys.stdin):
    sha = c['sha'][:12]
    msg = c['commit']['message'].split('\n')[0][:72]
    print(f'  {sha} {msg}')
" 2>/dev/null || echo "  (could not fetch commit history — check network/rate limit)"
echo ""

# Show diffs
echo "=== Diff: compat.bpf.h (upstream vs vendored) ==="
diff -u "$TMPDIR/compat.bpf.h" "$VENDORED_COMPAT" || true
echo ""

echo "=== Diff: core_fixes.bpf.h (upstream vs vendored) ==="
diff -u "$TMPDIR/core_fixes.bpf.h" "$VENDORED_CORE" || true
echo ""

echo "Review the diffs above and manually apply any upstream fixes to:"
echo "  $VENDORED_COMPAT"
echo "  $VENDORED_CORE"
echo ""
echo "Then update the 'Upstream commit' lines in the file headers."
