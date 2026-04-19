#!/usr/bin/env bash
# case-f: the git-wrapper intercepts a write attempt from a cwd that has
# not installed the core.hooksPath hooks — e.g. cadence-external tooling
# that drives git via GIT_DIR or an unrelated parent directory. The
# wrapper must refuse the write purely from shell-layer inspection.

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

REPO="$(setup_repo)"
EXTERNAL_CWD="$(mktemp -d -t wperf-hook-ext-XXXXXX)"
trap 'cleanup_repo "$REPO"; rm -rf "$EXTERNAL_CWD"' EXIT

# Disable the core.hooksPath inside REPO to simulate a clone / worktree
# that predates the β Step-0 wiring. Wrapper must still block.
git -C "$REPO" config --unset core.hooksPath 2>/dev/null || true
# Remove the hook symlinks so only the wrapper's check can fire.
rm -f "$REPO/.githooks/pre-commit" "$REPO/.githooks/commit-msg" "$REPO/.githooks/pre-push"

set_identity "$REPO" "Wenbo Zhang" "wenbo.zhang@iomesh.com"

echo "case-f" > "$REPO/case-f.txt"
git -C "$REPO" add case-f.txt

# Invoke via the wrapper from a foreign cwd, targeting the wperf repo
# through -C. The wrapper resolves repo_root via rev-parse and should
# refuse the commit.
if ( cd "$EXTERNAL_CWD" && "$REPO_ROOT/hooks/git-wrapper.sh" -C "$REPO" commit -m "feat: case-f wrapper bypass attempt" ) >/dev/null 2>&1; then
    printf '  FAIL: git-wrapper failed to block external-cwd commit under ethercflow\n' >&2
    exit 1
fi
exit 0
