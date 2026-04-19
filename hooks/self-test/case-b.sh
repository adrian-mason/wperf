#!/usr/bin/env bash
# case-b: duplicate Signed-off-by trailers are deduplicated.

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

REPO="$(setup_repo)"
msg_file="$(mktemp)"
trap 'rm -f "$msg_file"; cleanup_repo "$REPO"' EXIT

seed_commit "$REPO"

cat > "$msg_file" <<'EOF'
feat: case-b body

Signed-off-by: Adrian Mason <258563901+adrian-mason@users.noreply.github.com>
Signed-off-by: Adrian Mason <258563901+adrian-mason@users.noreply.github.com>
EOF

( cd "$REPO" && "$REPO_ROOT/hooks/dedup-trailers.sh" "$msg_file" )

count="$(grep -c '^Signed-off-by: Adrian Mason' "$msg_file" || true)"
if [[ "$count" != "1" ]]; then
    printf '  FAIL: expected 1 Signed-off-by, got %s\n' "$count" >&2
    cat "$msg_file" >&2
    exit 1
fi
exit 0
