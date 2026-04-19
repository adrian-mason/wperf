#!/usr/bin/env bash
# hooks/preflight-identity.sh — Diagnostic view of git identity for this repo
#
# Emits the Q3=A 3-line / 6-field identity matrix (one line per scope,
# each line exposing user.name and user.email → 3 × 2 = 6 fields):
#   [--global]   expected baseline (ethercflow on this machine; NOT checked)
#   [--local]    must resolve to Adrian for wperf; BLOCK signal if missing
#   [effective]  what `git commit` would actually write; must match Adrian
#
# Usage:
#   hooks/preflight-identity.sh            # diagnostic-only, always exit 0
#   hooks/preflight-identity.sh --strict   # exit 1 if [--local] or [effective] != Adrian
#
# Design intent reference:
#   docs/process/commit-hygiene.md Appendix "Preflight BLOCK RCA"
#   (L0 entry-point / L1(a) abstract context logic / L1(b) cadence impl /
#    L2 hook enforcement backstop)

set -u

ADRIAN_NAME="Adrian Mason"
ADRIAN_EMAIL="258563901+adrian-mason@users.noreply.github.com"

STRICT=0
if [[ "${1:-}" == "--strict" ]]; then
    STRICT=1
fi

fetch() {
    # $1 = scope flag (--global / --local / <empty for effective>)
    # $2 = key (user.name / user.email)
    if [[ -n "$1" ]]; then
        git config "$1" --get "$2" 2>/dev/null || true
    else
        git config --get "$2" 2>/dev/null || true
    fi
}

GLOBAL_NAME="$(fetch --global user.name)"
GLOBAL_EMAIL="$(fetch --global user.email)"
LOCAL_NAME="$(fetch --local user.name)"
LOCAL_EMAIL="$(fetch --local user.email)"
EFFECTIVE_NAME="$(fetch '' user.name)"
EFFECTIVE_EMAIL="$(fetch '' user.email)"

printf '[--global]   %s <%s>   (baseline, not checked)\n' \
    "${GLOBAL_NAME:-<unset>}" "${GLOBAL_EMAIL:-<unset>}"
printf '[--local]    %s <%s>\n' \
    "${LOCAL_NAME:-<unset>}" "${LOCAL_EMAIL:-<unset>}"
printf '[effective]  %s <%s>\n' \
    "${EFFECTIVE_NAME:-<unset>}" "${EFFECTIVE_EMAIL:-<unset>}"

status=0
reasons=()

if [[ -z "$LOCAL_NAME" || -z "$LOCAL_EMAIL" ]]; then
    reasons+=("no [--local] user.name/user.email set in this worktree")
    status=1
fi
if [[ "$EFFECTIVE_NAME" != "$ADRIAN_NAME" ]]; then
    reasons+=("[effective] user.name = '${EFFECTIVE_NAME}', expected '${ADRIAN_NAME}'")
    status=1
fi
if [[ "$EFFECTIVE_EMAIL" != "$ADRIAN_EMAIL" ]]; then
    reasons+=("[effective] user.email = '${EFFECTIVE_EMAIL}', expected '${ADRIAN_EMAIL}'")
    status=1
fi

if [[ $status -ne 0 ]]; then
    printf '\npreflight: identity check FAILED\n' >&2
    for r in "${reasons[@]}"; do
        printf '  - %s\n' "$r" >&2
    done
    printf '\nremediation:\n' >&2
    printf '  git config --local user.name  "%s"\n' "$ADRIAN_NAME" >&2
    printf '  git config --local user.email "%s"\n' "$ADRIAN_EMAIL" >&2
    printf '\nsee docs/process/commit-hygiene.md for onboarding paths.\n' >&2
fi

if [[ $STRICT -eq 0 ]]; then
    exit 0
fi
exit $status
