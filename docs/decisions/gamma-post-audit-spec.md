# γ Post-Audit Spec — Post-Hoc Verification for the Filter-Repo Rewrite

- **Status:** Proposed
- **Date:** 2026-04-19
- **Scope:** post-β merge; pre-authorised under Adrian's Q4=A P9(ii) ruling
  as a **parallel post-hoc audit**, not a per-case pre-approval.
- **Precondition:** the β Step-0 commit gate is merged, hooks are installed,
  and `hooks/self-test/run-all.sh` passes green on `main`.

## 1. Purpose

The γ phase rewrites historical commits whose author/committer metadata
references `wenbo.zhang@iomesh.com` and whose trailers carry
`Co-authored-by:` lines. The rewrite is a forced, non-reversible history
mutation. This spec defines the three independent post-hoc audit layers
that must execute in sequence after the rewrite before the result is
accepted as the new `main`.

## 2. Scope of Rewrite

The rewrite covers every ref reachable from `origin/main` as of the
`EXPECTED_MAIN_SHA` freeze captured at γ.0 start time. Concretely:

- rewrite author: any commit where `author.email == wenbo.zhang@iomesh.com`
  becomes Adrian with `258563901+adrian-mason@users.noreply.github.com`.
- rewrite committer: same substitution.
- rewrite trailer: every `Co-Authored-By:` line is removed; a single
  `Signed-off-by: Adrian Mason <…>` trailer is injected if missing.

Out of scope:
- PR-attached tags such as `archive/<branch>` are rewritten only if they
  reach into the rewritten range; annotations and tag messages are not
  rewritten.
- `pre-rewrite-snapshot` tag is **frozen** as a pre-γ ground-truth
  reference and is explicitly excluded from rewriting.

## 3. Three-Layer Post-Hoc Audit

Each layer must produce a machine-readable artefact committed under
`docs/audit/gamma/` before the next layer runs.

### γ.0 — Challenger Preflight (pre-rewrite baseline)

Responsibility: Challenger.
Inputs: current `origin/main`, `pre-rewrite-snapshot` tag.
Outputs:
- `gamma-0-expected-main-sha.txt` — the SHA pinned as
  `EXPECTED_MAIN_SHA`; every subsequent layer verifies git state against it.
- `gamma-0-identity-matrix.txt` — the concatenation of three 3-line /
  6-field matrices emitted by `hooks/preflight-identity.sh`, one per
  context: the two active worktrees plus the `pre-rewrite-snapshot`
  checkout. Confirms the machine's baseline is understood before the
  rewrite starts.

Execution flow (explicit, to avoid the single-cwd gotcha):

```
{
  cd <worktree-1>                     && hooks/preflight-identity.sh
  cd <worktree-2>                     && hooks/preflight-identity.sh
  cd <pre-rewrite-snapshot-checkout>  && hooks/preflight-identity.sh
} > gamma-0-identity-matrix.txt
```

Gate: `git rev-parse origin/main == EXPECTED_MAIN_SHA` must hold at the
moment the rewrite is launched. Any drift aborts γ.

### γ.3 — Oracle Trailer-Content Audit (post-rewrite)

Responsibility: Oracle.
Inputs: rewritten `main`, `EXPECTED_MAIN_SHA` (expected to no longer
resolve to a reachable ref).
Outputs:
- `gamma-3-author-refs.txt` — `git log --format='%H %an <%ae>' <range>`,
  asserted to contain zero occurrences of `wenbo.zhang@iomesh.com` and
  zero occurrences of any author other than Adrian.
- `gamma-3-trailer-refs.txt` — grep of commit bodies for
  `^Co-[Aa]uthored-[Bb]y:` (must be empty) and `^Signed-off-by:` (must
  be present on every commit with Adrian's canonical address).

Gate: both refs files must match the expected shape byte-for-byte. Any
stray address or missing trailer aborts γ.

### γ — Probe 4-Check Refs (post-γ.3, BPF-scope guard)

Responsibility: Probe.
Inputs: rewritten `main`, γ.3 artefacts.
Outputs:
- `gamma-4check-refs.txt` — four-column sanity table per
  rewritten commit: `sha | author_name | author_email | signoff_count`.
- `gamma-4check-summary.txt` — BPF-scope regression guard: the set of
  tracepoints, ADRs, and kprobe attach sites touched by any rewritten
  commit must match the pre-rewrite snapshot byte-for-byte (the rewrite
  is metadata-only; any code-path drift is a γ bug).

Gate: the 4-check table must show exactly one `Signed-off-by` trailer
pointing to Adrian per commit, and the BPF-scope diff against
`pre-rewrite-snapshot` must be empty.

## 4. Relationship to Q4=A Pre-Authorisation

Adrian's Q4=A ruling pre-authorises the **parallel** post-hoc audit
structure: γ.0 / γ.3 / γ(post-γ.3) may run in overlapping compose
windows among Challenger / Oracle / Probe, so long as each layer's
gate is checked against the frozen `EXPECTED_MAIN_SHA`. The pre-
authorisation covers the audit shape only; individual case decisions
during the audit still flow back to Adrian.

## 5. Failure Handling

Any gate failure at γ.0, γ.3, or γ(post-γ.3):

1. Freeze push access to `main` (branch-protection under Q1=B).
2. Publish the failing artefact into `docs/audit/gamma/` under
   `failure-<layer>-<timestamp>.txt`.
3. Open a `gamma-<layer>-regression` issue citing the failing artefact.
4. Hand off to Adrian; no automatic rollback.

`pre-rewrite-snapshot` is the authoritative pre-γ reference for any
recovery and must remain pushed to origin for the life of the project.

## 6. Out of Scope

- ε-phase `push --force-with-lease` choreography — tracked separately.
- θ-phase metadata backfill — depends on γ success and is specified in
  its own ADR.
- ζ.1 CI re-run — consumes γ outputs but is not part of the audit.
