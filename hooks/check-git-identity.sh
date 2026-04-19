#!/usr/bin/env bash
# hooks/check-git-identity.sh — Hook-mode strict identity BLOCK
#
# Installed as pre-commit and pre-push. Exits non-zero when the effective
# identity that git is about to record (author / committer / tag-pusher)
# is not Adrian Mason. Layer 1 + Layer 3 of the 4-layer matrix:
#   L1 pre-commit — blocks commits authored under wrong identity
#   L3 pre-push   — final gate before refs escape the worktree
#
# Reads identity from git config (which hook context resolves with
# worktree-local precedence), not from CLAUDE.md or environment.
#
# Design intent reference:
#   docs/process/commit-hygiene.md Appendix "Preflight BLOCK RCA"

set -u

ADRIAN_NAME="Adrian Mason"
ADRIAN_EMAIL="258563901+adrian-mason@users.noreply.github.com"

# GIT_AUTHOR_* / GIT_COMMITTER_* are set in some hook contexts; prefer them
# when present so we validate the exact identity about to be recorded.
name="${GIT_AUTHOR_NAME:-}"
email="${GIT_AUTHOR_EMAIL:-}"
if [[ -z "$name" ]]; then
    name="$(git config --get user.name 2>/dev/null || true)"
fi
if [[ -z "$email" ]]; then
    email="$(git config --get user.email 2>/dev/null || true)"
fi

hook_name="$(basename "${0}")"

fail() {
    printf '\n%s: identity check FAILED\n' "$hook_name" >&2
    printf '  effective: %s <%s>\n' "${name:-<unset>}" "${email:-<unset>}" >&2
    printf '  expected:  %s <%s>\n' "$ADRIAN_NAME" "$ADRIAN_EMAIL" >&2
    printf '\nremediation (run inside this worktree):\n' >&2
    printf '  git config --local user.name  "%s"\n' "$ADRIAN_NAME" >&2
    printf '  git config --local user.email "%s"\n' "$ADRIAN_EMAIL" >&2
    printf '\nsee docs/process/commit-hygiene.md Appendix "Preflight BLOCK RCA".\n' >&2
    exit 1
}

if [[ -z "$name" || -z "$email" ]]; then
    fail
fi
if [[ "$name" != "$ADRIAN_NAME" ]]; then
    fail
fi
if [[ "$email" != "$ADRIAN_EMAIL" ]]; then
    fail
fi

exit 0
