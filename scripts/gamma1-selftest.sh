#!/usr/bin/env bash
# γ.1 filter-repo callback self-test — 5-case byte-level adversarial
# corpus per Maestro msg=12535a11 §2 (locked alignment with Challenger
# msg=14ee4bc5 §1 + Oracle msg=2b0ae7d3 §4 projection).
#
# Exit 0 iff all 5 cases produce byte-exact expected output.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CALLBACK_PATH="$SCRIPT_DIR/gamma1_filter_repo.py"

CALLBACK_PATH="$CALLBACK_PATH" python3 - <<'PY'
import os
import sys
import importlib.util

path = os.environ['CALLBACK_PATH']
spec = importlib.util.spec_from_file_location('gamma1_filter_repo', path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

normalize_message = mod.normalize_message
A = mod.ADRIAN_CANONICAL
W = b'Signed-off-by: Wenbo Zhang <wenbo.zhang@iomesh.com>'
BODY = b'commit body text'
MERGE_BODY = (
    b"Merge pull request #117 from adrian-mason/feat/step-0-commit-gate"
    b"\n\nstep-0: commit-gate"
)

cases = [
    (1,
     BODY + b'\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n' + A,
     BODY + b'\n\n' + A),
    (2,
     BODY + b'\n\n' + W + b'\n' + A,
     BODY + b'\n\n' + A),
    (3,
     BODY + b'\n\n' + W,
     BODY + b'\n\n' + A),
    (4,
     BODY + b'\n\n' + A + b'\n' + A + b'\n' + A,
     BODY + b'\n\n' + A),
    (5,
     MERGE_BODY,
     MERGE_BODY + b'\n\n' + A),
]

fail = 0
for n, inp, expected in cases:
    got = normalize_message(inp)
    if got == expected:
        print(f"case {n}: PASS")
    else:
        fail += 1
        print(f"case {n}: FAIL")
        print(f"  input:    {inp!r}")
        print(f"  expected: {expected!r}")
        print(f"  got:      {got!r}")

if fail:
    print(f"\n{fail}/5 case(s) failed")
    sys.exit(1)
print("\n5/5 PASS")
PY
