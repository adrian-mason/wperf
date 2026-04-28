#!/usr/bin/env bash
# sync-libbpf-compat.sh — Sync compat.bpf.h + core_fixes.bpf.h from bcc/libbpf-tools
#
# This script documents the provenance of vendored BPF headers:
#   - src/bpf/compat.bpf.h  (reserve_buf/submit_buf transport abstraction)
#   - src/bpf/core_fixes.bpf.h (CO-RE field rename fixes)
#
# Vendored allowlist for core_fixes.bpf.h (subset of upstream — keep in sync
# with the "Vendored sections" comment in src/bpf/core_fixes.bpf.h):
#   - task_struct state/__state rename          (sched path)
#   - bio bi_disk/bi_bdev + get_gendisk()        (block layer, bio side)
#   - trace_event_raw_block_rq_complete[ion]    (block_rq_error struct rename)
#   - request rq_disk + request_queue->disk
#     + get_disk()                               (block layer, rq side — needed
#                                                 by Phase 2b #38 P2b-01)
# Upstream helpers NOT vendored: renamedata, kmem_alloc/free, inet_sock
# bitfields, cfs_rq nr_running. Add here + in core_fixes.bpf.h if/when used.
#
# Fetches directly from iovisor/bcc GitHub repo (no local clone needed).
# By default, fetches at the pinned upstream commits recorded in the vendored
# file headers. Pass --ref <sha-or-branch> to compare against a different ref.
#
# Vendored files are NOT verbatim copies — they are adapted for wPerf.
# When updating, diff the upstream changes and manually apply relevant fixes
# that fall within the allowlist above. Expansions to the allowlist require
# updating both this script's doc comment and core_fixes.bpf.h's header.
#
# Usage: ./scripts/sync-libbpf-compat.sh [--ref <sha-or-branch>]
#   Default: fetches at the pinned upstream commits from vendored file headers
#   --ref master: compare against latest upstream HEAD

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

VENDORED_COMPAT="$REPO_ROOT/src/bpf/compat.bpf.h"
VENDORED_CORE="$REPO_ROOT/src/bpf/core_fixes.bpf.h"

# Pinned upstream commits — must match the "Upstream commit:" lines in vendored headers
COMPAT_PIN="7f394c6d6775b9df68cac30b8147f9ab8a611ba7"
CORE_PIN="82ad428c40cb270fda6c0de5a9914705c94dd4c7"

# Parse args
REF_OVERRIDE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --ref)
            REF_OVERRIDE="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--ref <sha-or-branch>]"
            exit 1
            ;;
    esac
done

COMPAT_REF="${REF_OVERRIDE:-$COMPAT_PIN}"
CORE_REF="${REF_OVERRIDE:-$CORE_PIN}"

GITHUB_RAW="https://raw.githubusercontent.com/iovisor/bcc"
GITHUB_LOG="https://api.github.com/repos/iovisor/bcc/commits"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# Fetch upstream files at pinned (or overridden) refs
echo "=== Fetching upstream files from iovisor/bcc ==="
echo "  compat.bpf.h    @ ${COMPAT_REF:0:12}"
echo "  core_fixes.bpf.h @ ${CORE_REF:0:12}"
echo ""

curl -sfL "$GITHUB_RAW/$COMPAT_REF/libbpf-tools/compat.bpf.h" \
    -o "$TMPDIR/compat.bpf.h" || {
    echo "Error: failed to fetch compat.bpf.h at ref $COMPAT_REF"
    exit 1
}

curl -sfL "$GITHUB_RAW/$CORE_REF/libbpf-tools/core_fixes.bpf.h" \
    -o "$TMPDIR/core_fixes.bpf.h" || {
    echo "Error: failed to fetch core_fixes.bpf.h at ref $CORE_REF"
    exit 1
}
echo "  Downloaded both files"
echo ""

# Show upstream provenance via GitHub API
echo "=== Upstream provenance (recent commits) ==="
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
echo "=== Diff: compat.bpf.h (upstream @ ${COMPAT_REF:0:12} vs vendored) ==="
diff -u --label "upstream/compat.bpf.h" --label "vendored/compat.bpf.h" \
    "$TMPDIR/compat.bpf.h" "$VENDORED_COMPAT" || true
echo ""

echo "=== Diff: core_fixes.bpf.h (upstream @ ${CORE_REF:0:12} vs vendored) ==="
diff -u --label "upstream/core_fixes.bpf.h" --label "vendored/core_fixes.bpf.h" \
    "$TMPDIR/core_fixes.bpf.h" "$VENDORED_CORE" || true
echo ""

echo "Review the diffs above and manually apply any upstream fixes to:"
echo "  $VENDORED_COMPAT"
echo "  $VENDORED_CORE"
echo ""
echo "To check against latest upstream: $0 --ref master"
echo "Then update the 'Upstream commit' lines in the file headers and the"
echo "pinned SHAs in this script (COMPAT_PIN / CORE_PIN)."
