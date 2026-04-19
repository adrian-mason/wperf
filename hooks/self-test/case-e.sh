#!/usr/bin/env bash
# case-e: `git push` attempted under the ethercflow identity MUST be
# blocked by the pre-push hook, even if the commit being pushed was
# authored under the correct identity. The pre-push gate is the final
# backstop before refs escape the worktree.

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

REPO="$(setup_repo)"
BARE="$(mktemp -d -t wperf-hook-bare-XXXXXX)"
trap 'cleanup_repo "$REPO"; rm -rf "$BARE"' EXIT

git init --quiet --bare "$BARE"

# Author a legitimate commit under Adrian.
seed_commit "$REPO"
git -C "$REPO" remote add origin "$BARE"

# Now switch identity to the ethercflow baseline and attempt to push.
set_identity "$REPO" "Wenbo Zhang" "wenbo.zhang@iomesh.com"

git -C "$REPO" tag v-case-e
if git -C "$REPO" push --quiet origin main v-case-e >/dev/null 2>&1; then
    printf '  FAIL: pre-push failed to block ethercflow identity\n' >&2
    exit 1
fi
exit 0
