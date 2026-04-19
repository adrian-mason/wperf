#!/usr/bin/env bash
# case-a: Co-Authored-By lines are stripped by dedup-trailers.sh.

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

REPO="$(setup_repo)"
trap 'cleanup_repo "$REPO"' EXIT

seed_commit "$REPO"

msg_file="$(mktemp)"
trap 'rm -f "$msg_file"; cleanup_repo "$REPO"' EXIT
cat > "$msg_file" <<'EOF'
feat: case-a body

Signed-off-by: Adrian Mason <258563901+adrian-mason@users.noreply.github.com>
Co-Authored-By: Wenbo Zhang <wenbo.zhang@iomesh.com>
Co-authored-by: ChatGPT <noreply@openai.com>
EOF

( cd "$REPO" && "$REPO_ROOT/hooks/dedup-trailers.sh" "$msg_file" )

if grep -qi '^co-authored-by:' "$msg_file"; then
    printf '  FAIL: Co-authored-by not stripped\n' >&2
    cat "$msg_file" >&2
    exit 1
fi
if ! grep -q '^Signed-off-by: Adrian Mason' "$msg_file"; then
    printf '  FAIL: Signed-off-by Adrian lost\n' >&2
    cat "$msg_file" >&2
    exit 1
fi
exit 0
