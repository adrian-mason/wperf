#!/usr/bin/env bash
# hooks/git-wrapper.sh — Shell-layer identity gate (L4 / 4-layer matrix backstop)
#
# The hook layers (L1 pre-commit / L2 commit-msg / L3 pre-push) only fire
# when core.hooksPath is wired inside the target repo. They do not help
# when a user runs `git` from an external cwd against this worktree via
# GIT_DIR / GIT_WORK_TREE, or against an unrelated clone that lacks the
# hooks. The wrapper closes that gap by inspecting the cwd context at the
# outermost git invocation and refusing commit / push / tag operations that
# resolve into a wperf worktree without an Adrian [--local] override.
#
# Install by sourcing hooks/wrapper.fish (fish) or hooks/wrapper.bash (bash)
# from your shell init. The wrapper exports an alias `git` that delegates
# to this script, which then re-execs the real git after validation.
#
# This script is intentionally permissive for read-only subcommands
# (status, log, diff, fetch, show, config, etc.) so that it does not
# degrade interactive workflows. Only writing subcommands go through the
# identity gate.

set -u

ADRIAN_NAME="Adrian Mason"
ADRIAN_EMAIL="258563901+adrian-mason@users.noreply.github.com"

# Resolve the real git (skip any shell aliases of the same name).
REAL_GIT="$(command -v git.real 2>/dev/null || true)"
if [[ -z "$REAL_GIT" ]]; then
    # Scan PATH entries for a git that is not this wrapper.
    self="$(readlink -f "${BASH_SOURCE[0]}" 2>/dev/null || echo "${0}")"
    IFS=':' read -r -a path_entries <<< "$PATH"
    for dir in "${path_entries[@]}"; do
        candidate="$dir/git"
        if [[ -x "$candidate" ]]; then
            resolved="$(readlink -f "$candidate" 2>/dev/null || echo "$candidate")"
            if [[ "$resolved" != "$self" ]]; then
                REAL_GIT="$candidate"
                break
            fi
        fi
    done
fi
if [[ -z "$REAL_GIT" ]]; then
    printf 'git-wrapper: unable to locate real git binary\n' >&2
    exit 127
fi

# Pre-scan the global options that precede the subcommand. Git accepts
# -C <path> and -C<path> (and several other global flags) before the
# subcommand, so we have to skip them to find the real command name.
target_dir=""
i=1
while [[ $i -le $# ]]; do
    arg="${!i}"
    case "$arg" in
        -C)
            i=$((i + 1))
            if [[ $i -le $# ]]; then
                target_dir="${!i}"
            fi
            ;;
        -C*)
            target_dir="${arg#-C}"
            ;;
        -c|--namespace|--git-dir|--work-tree|--super-prefix|--exec-path|--list-cmds)
            i=$((i + 1))
            ;;
        -c=*|--namespace=*|--git-dir=*|--work-tree=*|--super-prefix=*|--exec-path=*)
            ;;
        -*)
            ;;
        *)
            subcommand="$arg"
            break
            ;;
    esac
    i=$((i + 1))
done
subcommand="${subcommand:-}"

case "$subcommand" in
    commit|merge|rebase|cherry-pick|revert|tag|push|am|apply|stash)
        enforce=1
        ;;
    *)
        enforce=0
        ;;
esac

if [[ $enforce -eq 0 ]]; then
    exec "$REAL_GIT" "$@"
fi

# Resolve the repo that the about-to-run git would operate on. Respect
# -C <dir> (case-f: cadence-external cwd) and GIT_DIR / GIT_WORK_TREE.
if [[ -n "$target_dir" ]]; then
    repo_root="$("$REAL_GIT" -C "$target_dir" rev-parse --show-toplevel 2>/dev/null || true)"
else
    repo_root="$("$REAL_GIT" rev-parse --show-toplevel 2>/dev/null || true)"
fi
if [[ -z "$repo_root" ]]; then
    # Not inside a git repo: let real git produce its own error.
    exec "$REAL_GIT" "$@"
fi

# Heuristic: is this a wperf worktree? Look for the canonical manifest.
if [[ ! -f "$repo_root/Cargo.toml" ]] || ! grep -q '^name = "wperf"' "$repo_root/Cargo.toml" 2>/dev/null; then
    exec "$REAL_GIT" "$@"
fi

name="$("$REAL_GIT" -C "$repo_root" config --get user.name 2>/dev/null || true)"
email="$("$REAL_GIT" -C "$repo_root" config --get user.email 2>/dev/null || true)"

if [[ "$name" != "$ADRIAN_NAME" || "$email" != "$ADRIAN_EMAIL" ]]; then
    printf '\ngit-wrapper: refusing "git %s" — identity mismatch\n' "$subcommand" >&2
    printf '  repo:       %s\n' "$repo_root" >&2
    printf '  effective:  %s <%s>\n' "${name:-<unset>}" "${email:-<unset>}" >&2
    printf '  expected:   %s <%s>\n' "$ADRIAN_NAME" "$ADRIAN_EMAIL" >&2
    printf '\nrun inside the worktree:\n' >&2
    printf '  git config --local user.name  "%s"\n' "$ADRIAN_NAME" >&2
    printf '  git config --local user.email "%s"\n' "$ADRIAN_EMAIL" >&2
    printf '\nsee docs/process/commit-hygiene.md Appendix "Preflight BLOCK RCA".\n' >&2
    exit 1
fi

exec "$REAL_GIT" "$@"
