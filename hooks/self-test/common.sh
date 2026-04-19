#!/usr/bin/env bash
# hooks/self-test/common.sh — Shared test harness helpers
#
# Sourced by each case-*.sh script. Provides:
#   - setup_repo: create a temp repo that mirrors the wperf hook layout
#   - cleanup_repo: remove the temp repo
#   - set_identity: write --local user.name / user.email
#   - expect_success / expect_block: run a command and assert exit code

set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

setup_repo() {
    local dir
    dir="$(mktemp -d -t wperf-hook-test-XXXXXX)"
    git init --quiet -b main "$dir"
    # Seed a Cargo.toml so git-wrapper.sh recognises the repo as wperf.
    cat > "$dir/Cargo.toml" <<'EOF'
[package]
name = "wperf"
version = "0.0.0"
edition = "2021"
EOF
    # Install hooks via core.hooksPath, pointing at a copy of this repo's
    # .githooks/ directory with the hooks/*.sh implementations resolved
    # against REPO_ROOT.
    mkdir -p "$dir/.githooks"
    ln -s "$REPO_ROOT/hooks/check-git-identity.sh" "$dir/.githooks/pre-commit"
    ln -s "$REPO_ROOT/hooks/dedup-trailers.sh"     "$dir/.githooks/commit-msg"
    ln -s "$REPO_ROOT/hooks/check-git-identity.sh" "$dir/.githooks/pre-push"
    git -C "$dir" config core.hooksPath .githooks
    # Isolate from any global hooks or pager config that could interfere.
    git -C "$dir" config commit.gpgsign false
    git -C "$dir" config tag.gpgsign false
    printf '%s' "$dir"
}

cleanup_repo() {
    local dir="${1:-}"
    if [[ -n "$dir" && -d "$dir" ]]; then
        rm -rf "$dir"
    fi
}

set_identity() {
    # $1 = repo dir, $2 = name, $3 = email
    git -C "$1" config --local user.name  "$2"
    git -C "$1" config --local user.email "$3"
}

seed_commit() {
    # Create an initial commit so later operations have a parent.
    local dir="$1"
    set_identity "$dir" "Adrian Mason" "258563901+adrian-mason@users.noreply.github.com"
    echo "seed" > "$dir/seed.txt"
    git -C "$dir" add seed.txt
    git -C "$dir" commit --quiet -m "seed" >/dev/null
}

expect_success() {
    # $1 = description, $2..$@ = command
    local desc="$1"; shift
    if ! "$@" >/dev/null 2>&1; then
        printf '  FAIL: expected success but got exit %d: %s\n' "$?" "$desc" >&2
        return 1
    fi
    return 0
}

expect_block() {
    # $1 = description, $2..$@ = command
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        printf '  FAIL: expected block but command succeeded: %s\n' "$desc" >&2
        return 1
    fi
    return 0
}
