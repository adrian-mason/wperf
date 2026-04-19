#!/usr/bin/env bash
# case-h: wrapper envvar defense-in-depth (case-f-class with envvar attack).
#
# Setup: stored [--local] identity is clean Adrian, hooks are disabled
# (core.hooksPath unset + .githooks/ symlinks removed), and the commit is
# invoked through the wrapper from an external cwd with
# GIT_COMMITTER_NAME / GIT_COMMITTER_EMAIL = ethercflow. This is the exact
# class of case (f) — hooks are not the guard — but the attack vector is
# the env-var path rather than the stored-config path. Without the
# wrapper's envvar read (fix-pass 2.1), the wrapper's stored-config check
# would PASS (stored is Adrian) and the commit would succeed with the
# wrong committer recorded.
#
# This is the direct regression guard for the fix-pass 2.1 defense-in-depth
# code in git-wrapper.sh. If case-h regresses, the wrapper has lost its
# envvar-attack coverage and case-f-class scenarios (hooks disabled) fall
# through to git with an ethercflow committer.

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

REPO="$(setup_repo)"
EXTERNAL_CWD="$(mktemp -d -t wperf-hook-ext-h-XXXXXX)"
stderr_file="$(mktemp)"
trap 'cleanup_repo "$REPO"; rm -rf "$EXTERNAL_CWD"; rm -f "$stderr_file"' EXIT

# Disable hooks to isolate the wrapper as the only guard (case-f parity).
git -C "$REPO" config --unset core.hooksPath 2>/dev/null || true
rm -f "$REPO/.githooks/pre-commit" "$REPO/.githooks/commit-msg" "$REPO/.githooks/pre-push"

# Stored config clean Adrian — the critical distinction from case (f),
# which poisons [--local]. Here the stored path is blameless; the attack
# surface is the committer env-var.
set_identity "$REPO" "Adrian Mason" "258563901+adrian-mason@users.noreply.github.com"

echo "case-h" > "$REPO/case-h.txt"
git -C "$REPO" add case-h.txt

if ( cd "$EXTERNAL_CWD" && \
     GIT_COMMITTER_NAME="Wenbo Zhang" \
     GIT_COMMITTER_EMAIL="wenbo.zhang@iomesh.com" \
     "$REPO_ROOT/hooks/git-wrapper.sh" -C "$REPO" commit \
         -m "feat: case-h wrapper envvar bypass attempt" ) \
     >/dev/null 2>"$stderr_file"; then
    printf '  FAIL: git-wrapper failed to block external-cwd commit with ethercflow committer envvar\n' >&2
    cat "$stderr_file" >&2
    exit 1
fi

if ! grep -q 'git-wrapper: refusing' "$stderr_file"; then
    printf '  FAIL: commit was blocked, but not by git-wrapper.sh\n' >&2
    printf '         (stderr did not contain "git-wrapper: refusing" — possible\n' >&2
    printf '          false-PASS: another layer may have fired unexpectedly)\n' >&2
    cat "$stderr_file" >&2
    exit 1
fi

if ! grep -q 'committer' "$stderr_file"; then
    printf '  FAIL: wrapper fired, but not on the committer role\n' >&2
    printf '         (stderr did not name "committer" — the stored/author check\n' >&2
    printf '          fired instead, meaning wrapper envvar committer path is\n' >&2
    printf '          still untested; case (f) already covers stored-config blocks)\n' >&2
    cat "$stderr_file" >&2
    exit 1
fi
exit 0
