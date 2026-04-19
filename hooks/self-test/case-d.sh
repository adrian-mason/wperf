#!/usr/bin/env bash
# case-d: commit attempted under the ethercflow identity (Wenbo Zhang
# <wenbo.zhang@iomesh.com>) MUST be blocked by the pre-commit hook.
#
# LOAD-BEARING — this case is the primary regression guard for the PR #111
# incident, where the absence of a per-repo [--local] override caused the
# machine's global ethercflow identity to author a wperf commit.
# Failure here = the entire commit gate has regressed.

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

REPO="$(setup_repo)"
trap 'cleanup_repo "$REPO"' EXIT

# NOTE: do not seed_commit with Adrian first — we want the very first commit
# attempt to be under the wrong identity, exactly as the #111 incident did.
set_identity "$REPO" "Wenbo Zhang" "wenbo.zhang@iomesh.com"

echo "case-d" > "$REPO/case-d.txt"
git -C "$REPO" add case-d.txt

# Capture stderr and assert BOTH (a) the commit exited non-zero AND
# (b) stderr carries the hook's characteristic BLOCK message. The
# stderr grep is a positive control — without it, an unrelated future
# failure (e.g. setup_repo regressing hooksPath wiring) could produce a
# false PASS that silently masks a PR #111-shaped regression.
stderr_file="$(mktemp)"
trap 'cleanup_repo "$REPO"; rm -f "$stderr_file"' EXIT

if git -C "$REPO" commit --quiet -m "feat: case-d ethercflow commit" >/dev/null 2>"$stderr_file"; then
    printf '  FAIL: pre-commit failed to block ethercflow identity\n' >&2
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
exit 0
