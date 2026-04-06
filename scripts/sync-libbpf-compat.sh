#!/usr/bin/env bash
# sync-libbpf-compat.sh — Sync compat.bpf.h from bcc/libbpf-tools
#
# This script documents the provenance of src/bpf/compat.bpf.h.
# The vendored file is NOT a verbatim copy — it is adapted for wPerf:
#   - Map declarations are in wperf.bpf.c (wperf-specific types/sizes)
#   - Drop counter instrumentation added to reserve_buf()
#
# When updating, diff the upstream changes and manually apply relevant
# fixes to src/bpf/compat.bpf.h.
#
# Usage: ./scripts/sync-libbpf-compat.sh [bcc-repo-path]

set -euo pipefail

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

# Show diff between upstream and our vendored version
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
VENDORED_FILE="$REPO_ROOT/src/bpf/compat.bpf.h"

echo "=== Diff: upstream vs vendored ==="
diff -u "$UPSTREAM_FILE" "$VENDORED_FILE" || true
echo ""
echo "Review the diff above and manually apply any upstream fixes to:"
echo "  $VENDORED_FILE"
echo ""
echo "Then update the 'Upstream commit' line in the file header to:"
echo "  $CURRENT_COMMIT"
