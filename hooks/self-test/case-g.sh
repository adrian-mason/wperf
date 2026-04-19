#!/usr/bin/env bash
# case-g: commit attempted with stored [--local] identity = Adrian BUT
# GIT_COMMITTER_NAME / GIT_COMMITTER_EMAIL env vars set to the ethercflow
# identity. Pre-commit MUST block — the committer env vars override the
# stored config at commit-record time, producing the same structural shape
# as the PR #111 regression (recorded identity ≠ intended identity) via an
# env-var path rather than a [--local] path.
#
# LOAD-BEARING — complements case (d) on the orthogonal envvar axis. The
# initial Item 10 gate checked only GIT_AUTHOR_* and fell back to user.*;
# GIT_COMMITTER_* was not read, so this attack vector was unguarded until
# fix-pass 2 (Gemini HIGH, check-git-identity.sh committer bypass).

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

REPO="$(setup_repo)"
stderr_file="$(mktemp)"
trap 'cleanup_repo "$REPO"; rm -f "$stderr_file"' EXIT

# Stored config is clean (Adrian) — this is the critical setup distinction
# from case (d), which poisons [--local] directly. Here the [--local] path
# is blameless; the attack surface is the env-var override.
set_identity "$REPO" "Adrian Mason" "258563901+adrian-mason@users.noreply.github.com"

echo "case-g" > "$REPO/case-g.txt"
git -C "$REPO" add case-g.txt

if GIT_COMMITTER_NAME="Wenbo Zhang" \
   GIT_COMMITTER_EMAIL="wenbo.zhang@iomesh.com" \
   git -C "$REPO" commit --quiet -m "feat: case-g committer-envvar commit" \
       >/dev/null 2>"$stderr_file"; then
    printf '  FAIL: pre-commit failed to block ethercflow committer envvar\n' >&2
    cat "$stderr_file" >&2
    exit 1
fi

if ! grep -q 'identity check FAILED' "$stderr_file"; then
    printf '  FAIL: commit was blocked, but not by check-git-identity.sh\n' >&2
    printf '         (stderr did not contain "identity check FAILED" — possible\n' >&2
    printf '          false-PASS: hooks may be misconfigured)\n' >&2
    cat "$stderr_file" >&2
    exit 1
fi

if ! grep -q 'committer' "$stderr_file"; then
    printf '  FAIL: identity check fired, but not on the committer role\n' >&2
    printf '         (stderr did not contain "committer" — the author check\n' >&2
    printf '          fired instead, meaning GIT_COMMITTER_* path is still\n' >&2
    printf '          untested; case (d) already covers author-only blocks)\n' >&2
    cat "$stderr_file" >&2
    exit 1
fi
exit 0
