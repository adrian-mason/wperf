#!/usr/bin/env bash
# sync-libbpf-compat.sh — Sync compat.bpf.h + core_fixes.bpf.h from bcc/libbpf-tools
#
# This script documents the provenance of vendored BPF headers:
#   - src/bpf/compat.bpf.h  (reserve_buf/submit_buf transport abstraction)
#   - src/bpf/core_fixes.bpf.h (CO-RE field rename fixes, e.g. state/__state)
#
# Vendored files are NOT verbatim copies — they are adapted for wPerf.
# When updating, diff the upstream changes and manually apply relevant fixes.
#
# Usage: ./scripts/sync-libbpf-compat.sh [bcc-repo-path]

set -euo pipefail

# Resolve paths before any cd
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
VENDORED_COMPAT="$REPO_ROOT/src/bpf/compat.bpf.h"

BCC_REPO="${1:-/workspace/kernel/bcc}"
UPSTREAM_FILE="$BCC_REPO/libbpf-tools/compat.bpf.h"

if [[ ! -f "$UPSTREAM_FILE" ]]; then
    echo "Error: $UPSTREAM_FILE not found"
    echo "Usage: $0 [path-to-bcc-repo]"
    exit 1
fi

# Show upstream provenance
echo "=== Upstream provenance ==="
cd "$BCC_REPO"
git log --oneline -3 -- libbpf-tools/compat.bpf.h
echo ""

CURRENT_COMMIT=$(git log --format='%H' -1 -- libbpf-tools/compat.bpf.h)
echo "Latest upstream commit: $CURRENT_COMMIT"
echo ""

echo "=== Diff: compat.bpf.h (upstream vs vendored) ==="
diff -u "$UPSTREAM_FILE" "$VENDORED_COMPAT" || true
echo ""

# core_fixes.bpf.h
UPSTREAM_CORE="$BCC_REPO/libbpf-tools/core_fixes.bpf.h"
VENDORED_CORE="$REPO_ROOT/src/bpf/core_fixes.bpf.h"

if [[ -f "$UPSTREAM_CORE" ]]; then
    echo "=== core_fixes.bpf.h provenance ==="
    git log --oneline -3 -- libbpf-tools/core_fixes.bpf.h
    CORE_COMMIT=$(git log --format='%H' -1 -- libbpf-tools/core_fixes.bpf.h)
    echo "Latest upstream commit: $CORE_COMMIT"
    echo ""
    echo "=== Diff: core_fixes.bpf.h (upstream vs vendored) ==="
    diff -u "$UPSTREAM_CORE" "$VENDORED_CORE" || true
    echo ""
fi

echo "Review the diffs above and manually apply any upstream fixes to:"
echo "  $VENDORED_COMPAT"
echo "  $VENDORED_CORE"
echo ""
echo "Then update the 'Upstream commit' lines in the file headers."
