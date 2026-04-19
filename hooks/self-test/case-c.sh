#!/usr/bin/env bash
# case-c: a clean, well-formed commit message passes through unchanged
# (apart from idempotent Signed-off-by injection when missing).

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

REPO="$(setup_repo)"
msg_file="$(mktemp)"
trap 'rm -f "$msg_file"; cleanup_repo "$REPO"' EXIT

seed_commit "$REPO"

cat > "$msg_file" <<'EOF'
feat: case-c body line one

body line two

Signed-off-by: Adrian Mason <258563901+adrian-mason@users.noreply.github.com>
EOF

expected="$(cat "$msg_file")"

( cd "$REPO" && "$REPO_ROOT/hooks/dedup-trailers.sh" "$msg_file" )

actual="$(cat "$msg_file")"
if [[ "$actual" != "$expected" ]]; then
    printf '  FAIL: clean message mutated by dedup-trailers\n' >&2
    diff <(printf '%s' "$expected") <(printf '%s' "$actual") >&2 || true
    exit 1
fi
exit 0
