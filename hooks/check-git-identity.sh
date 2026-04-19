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

# Git records two identities per commit: author and committer. Each can be
# overridden independently via GIT_AUTHOR_* or GIT_COMMITTER_* env vars; when
# an env var is unset git falls back to user.name / user.email. We must
# validate BOTH pairs — a commit whose author matches Adrian but whose
# committer is Wenbo (or vice versa) is the same structural shape as the
# PR #111 regression and must be refused identically.
stored_name="$(git config --get user.name 2>/dev/null || true)"
stored_email="$(git config --get user.email 2>/dev/null || true)"

author_name="${GIT_AUTHOR_NAME:-$stored_name}"
author_email="${GIT_AUTHOR_EMAIL:-$stored_email}"
committer_name="${GIT_COMMITTER_NAME:-$stored_name}"
committer_email="${GIT_COMMITTER_EMAIL:-$stored_email}"

hook_name="$(basename "${0}")"

fail() {
    local role="$1" n="$2" e="$3"
    printf '\n%s: identity check FAILED (%s)\n' "$hook_name" "$role" >&2
    printf '  effective: %s <%s>\n' "${n:-<unset>}" "${e:-<unset>}" >&2
    printf '  expected:  %s <%s>\n' "$ADRIAN_NAME" "$ADRIAN_EMAIL" >&2
    printf '\nremediation (run inside this worktree):\n' >&2
    printf '  git config --local user.name  "%s"\n' "$ADRIAN_NAME" >&2
    printf '  git config --local user.email "%s"\n' "$ADRIAN_EMAIL" >&2
    printf '  unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL\n' >&2
    printf '\nsee docs/process/commit-hygiene.md Appendix "Preflight BLOCK RCA".\n' >&2
    exit 1
}

check_role() {
    local role="$1" n="$2" e="$3"
    if [[ -z "$n" || -z "$e" ]]; then
        fail "$role" "$n" "$e"
    fi
    if [[ "$n" != "$ADRIAN_NAME" || "$e" != "$ADRIAN_EMAIL" ]]; then
        fail "$role" "$n" "$e"
    fi
}

check_role "author"    "$author_name"    "$author_email"
check_role "committer" "$committer_name" "$committer_email"

exit 0
