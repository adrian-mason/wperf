#!/usr/bin/env python3
"""γ.1 filter-repo callback — canonical per Challenger msg=8698f3cf §3
+ Oracle msg=2b0ae7d3 §1 ordering (i)->(ii.a)->(ii.b)->(iii).

Scope per docs/decisions/gamma-post-audit-spec.md §2:
- author/committer: wenbo.zhang@iomesh.com -> Adrian canonical
- trailer: every Co-Authored-By stripped; resulting trailer block
  contains exactly one canonical Adrian Signed-off-by (inject if
  missing, dedup if multiple, strip if non-canonical address);
  applied uniformly to merge and non-merge commits.
- refs: refs/heads/main (pre-rewrite-snapshot + pr111-contaminated-snapshot
  tags explicitly excluded per §2 L34-35).

normalize_message is importable; scripts/gamma1-selftest.sh exercises it
against the 5-case adversarial corpus per Maestro msg=12535a11 §2.

When run as __main__, this file invokes git-filter-repo via subprocess
and re-executes itself inside the callback body to share normalize_message
(single source of truth — no inline duplication of the callback logic).

Filename dashes->underscores deviation from plan filename convention is
intentional per Oracle msg=11b79352 §2 RULING precedent (content
load-bearing, filename non-load-bearing); required for Python importability
so selftest + filter-repo callback share a single normalize_message
implementation.
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

WENBO_EMAIL = b'wenbo.zhang@iomesh.com'
ADRIAN_NAME = b'Adrian Mason'
ADRIAN_EMAIL = b'258563901+adrian-mason@users.noreply.github.com'
ADRIAN_CANONICAL = (
    b'Signed-off-by: Adrian Mason <258563901+adrian-mason@users.noreply.github.com>'
)

_CO_AUTHORED_BY_RE = re.compile(rb'^Co-Authored-By:', re.IGNORECASE)
_SIGNED_OFF_BY_RE = re.compile(rb'^Signed-off-by:', re.IGNORECASE)


def normalize_message(message: bytes) -> bytes:
    """Apply (i)->(ii.a)->(ii.b)->(iii) trailer normalization.

    (i)    strip Co-Authored-By: (key IGNORECASE per RFC 5322 §2.2.3)
    (ii.a) strip non-Adrian Signed-off-by: (byte-exact != ADRIAN_CANONICAL)
    (ii.b) dedup Adrian-canonical Signed-off-by: to the first occurrence
    (iii)  if 0 Adrian signoff remain, append ADRIAN_CANONICAL
    """
    lines = message.split(b'\n')
    lines = [l for l in lines if not _CO_AUTHORED_BY_RE.match(l)]
    seen_adrian = False
    out = []
    for l in lines:
        if _SIGNED_OFF_BY_RE.match(l):
            if l.rstrip() == ADRIAN_CANONICAL:
                if seen_adrian:
                    continue
                seen_adrian = True
                out.append(l)
            continue
        out.append(l)
    if not seen_adrian:
        if out and out[-1].strip():
            out.append(b'')
        out.append(ADRIAN_CANONICAL)
    return b'\n'.join(out)


def _build_message_callback_body(module_path: Path) -> str:
    return (
        "import importlib.util\n"
        f"_spec = importlib.util.spec_from_file_location('g1', {str(module_path)!r})\n"
        "_m = importlib.util.module_from_spec(_spec)\n"
        "_spec.loader.exec_module(_m)\n"
        "return _m.normalize_message(message)\n"
    )


def _build_commit_callback_body(module_path: Path) -> str:
    return (
        "import importlib.util\n"
        f"_spec = importlib.util.spec_from_file_location('g1', {str(module_path)!r})\n"
        "_m = importlib.util.module_from_spec(_spec)\n"
        "_spec.loader.exec_module(_m)\n"
        "if commit.author_email == _m.WENBO_EMAIL:\n"
        "    commit.author_email = _m.ADRIAN_EMAIL\n"
        "    commit.author_name = _m.ADRIAN_NAME\n"
        "if commit.committer_email == _m.WENBO_EMAIL:\n"
        "    commit.committer_email = _m.ADRIAN_EMAIL\n"
        "    commit.committer_name = _m.ADRIAN_NAME\n"
    )


def main(argv: list[str]) -> int:
    module_path = Path(__file__).resolve()
    msg_body = _build_message_callback_body(module_path)
    commit_body = _build_commit_callback_body(module_path)

    cmd = [
        'git-filter-repo',
        '--force',
        '--refs', 'refs/heads/main',
        '--partial',
        '--message-callback', msg_body,
        '--commit-callback', commit_body,
    ] + argv[1:]

    if os.environ.get('GAMMA1_DRY_RUN'):
        cmd.insert(1, '--dry-run')

    result = subprocess.run(cmd)
    return result.returncode


if __name__ == '__main__':
    sys.exit(main(sys.argv))
