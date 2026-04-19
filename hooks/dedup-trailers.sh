#!/usr/bin/env bash
# hooks/dedup-trailers.sh — Clean up commit message trailers (L2, commit-msg hook)
#
# - Removes Co-Authored-By / Co-authored-by lines entirely
# - Deduplicates other standard git trailers (Signed-off-by, etc.)
# - Strips separator lines (e.g. "---" or "---------")
# - Injects Signed-off-by if missing, using git config user.{name,email}
#
# Usage as a git commit-msg hook:
#   .githooks/commit-msg -> ../hooks/dedup-trailers.sh
#
# Can also be used standalone:
#   hooks/dedup-trailers.sh <commit-msg-file>
#
# Exit codes:
#   0 — always (rewrites in place, never blocks commits)

set -euo pipefail

COMMIT_MSG_FILE="${1:?usage: dedup-trailers.sh <commit-msg-file>}"

if [[ ! -f "$COMMIT_MSG_FILE" ]]; then
    echo "dedup-trailers: file not found: $COMMIT_MSG_FILE" >&2
    exit 0
fi

COAUTHOR_RE='^[Cc]o-[Aa]uthored-[Bb]y:[ \t]+'
SEPARATOR_RE='^-{3,}[[:space:]]*$'
TRAILER_RE='^(Signed-off-by|Acked-by|Reviewed-by|Tested-by|Reported-by|Helped-by|Cc):[ \t]+'

tmp="$(mktemp)"
trap 'rm -f "$tmp" "${tmp}.squeezed"' EXIT

seen_trailers=()

is_duplicate_trailer() {
    local line="$1"
    local normalized
    normalized="$(echo "$line" | tr '[:upper:]' '[:lower:]' | sed 's/[[:space:]]\+/ /g; s/[[:space:]]*$//')"
    for seen in "${seen_trailers[@]+"${seen_trailers[@]}"}"; do
        if [[ "$seen" == "$normalized" ]]; then
            return 0
        fi
    done
    seen_trailers+=("$normalized")
    return 1
}

while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" =~ $COAUTHOR_RE ]]; then
        continue
    fi
    if [[ "$line" =~ $SEPARATOR_RE ]]; then
        continue
    fi
    if [[ "$line" =~ $TRAILER_RE ]]; then
        if is_duplicate_trailer "$line"; then
            continue
        fi
    fi
    printf '%s\n' "$line"
done < "$COMMIT_MSG_FILE" > "$tmp"

cat -s "$tmp" > "${tmp}.squeezed" && mv "${tmp}.squeezed" "$tmp"

sed -i -e :a -e '/^\n*$/{$d;N;ba' -e '}' "$tmp"

SOB_NAME="$(git config user.name 2>/dev/null || true)"
SOB_EMAIL="$(git config user.email 2>/dev/null || true)"
if [[ -n "$SOB_NAME" && -n "$SOB_EMAIL" ]]; then
    SOB_LINE="Signed-off-by: ${SOB_NAME} <${SOB_EMAIL}>"
    if ! grep -qF "$SOB_LINE" "$tmp"; then
        printf '\n%s\n' "$SOB_LINE" >> "$tmp"
    fi
fi

cp "$tmp" "$COMMIT_MSG_FILE"
