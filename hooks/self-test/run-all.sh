#!/usr/bin/env bash
# hooks/self-test/run-all.sh — Execute all 7 regression cases (a–g) for the
# Item 10 commit gate.
#
# Cases:
#   a = Co-Authored-By strip            (commit-msg / dedup-trailers.sh)
#   b = Signed-off-by dedup             (commit-msg / dedup-trailers.sh)
#   c = clean base passthrough          (commit-msg / dedup-trailers.sh)
#   d = wenbo email commit BLOCK        (pre-commit / check-git-identity.sh)
#                                        LOAD-BEARING primary regression guard
#                                        for PR #111 incident ([--local] path)
#   e = wenbo email tag+push BLOCK      (pre-push / check-git-identity.sh)
#   f = cadence-external wrapper BLOCK  (git-wrapper.sh)
#   g = GIT_COMMITTER_* envvar BLOCK    (pre-commit / check-git-identity.sh)
#                                        LOAD-BEARING envvar-path complement
#                                        to case (d) — PR #111 same-shape via
#                                        committer env-var attack surface
#
# Usage:
#   hooks/self-test/run-all.sh
#
# Exit 0 if all six cases PASS, non-zero otherwise.

set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PASS=0
FAIL=0
FAILED_CASES=()

run_case() {
    local name="$1"
    local script="$HERE/case-${name}.sh"
    if [[ ! -x "$script" ]]; then
        printf 'MISSING  case-%s: %s\n' "$name" "$script" >&2
        FAIL=$((FAIL + 1))
        FAILED_CASES+=("$name")
        return
    fi
    if "$script"; then
        printf 'PASS     case-%s\n' "$name"
        PASS=$((PASS + 1))
    else
        printf 'FAIL     case-%s\n' "$name" >&2
        FAIL=$((FAIL + 1))
        FAILED_CASES+=("$name")
    fi
}

for c in a b c d e f g; do
    run_case "$c"
done

printf '\n--- summary ---\n'
printf 'pass: %d / 7\n' "$PASS"
printf 'fail: %d / 7\n' "$FAIL"

if [[ $FAIL -ne 0 ]]; then
    printf 'failed: %s\n' "${FAILED_CASES[*]}" >&2
    exit 1
fi
exit 0
