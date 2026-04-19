# Commit Hygiene — Item 10 Commit Gate

Status: **Active**
Last updated: 2026-04-19

This document is the contributor-facing companion to the β Step-0 commit
gate. It covers:

1. Why the gate exists (PR #111 incident, Q3=A ruling).
2. Onboarding paths (PRIMARY / SECONDARY).
3. What the four hook layers enforce at commit / push time.
4. The six-case self-test harness and how to run it.
5. Appendix — "Preflight BLOCK RCA" (canonical L0 / L1(a) / L1(b) / L2).

## 1. Background

`wperf` is developed on a machine shared with an ethercflow-owned repository.
The machine's `~/.gitconfig` sets `user.name = Wenbo Zhang` /
`user.email = wenbo.zhang@iomesh.com` **by design** — that is the correct
identity for the ethercflow repo. Per the Q3=A ruling, the machine-global
baseline is **not** modified to match Adrian; the per-repo `[--local]`
override is the sole enforcement surface for wperf.

PR #111 merged with a commit authored under the ethercflow identity because
the per-repo override was never set. The Item 10 commit gate closes that
gap with four independent enforcement layers, any one of which will refuse
the write.

## 2. Onboarding Paths

### PRIMARY — cadence worktree

```
# From anywhere inside the wperf main checkout
git worktree add ../wperf-<task> -b <branch> origin/main
cd ../wperf-<task>
git config --local user.name  "Adrian Mason"
git config --local user.email "258563901+adrian-mason@users.noreply.github.com"
git config --local core.hooksPath .githooks
hooks/preflight-identity.sh --strict
```

This is the supported path and the only one the reviewer team validates.
The `--strict` preflight run must exit 0 before the first commit.

### SECONDARY — manual clone (use only when cadence is unavailable)

```
git clone https://github.com/adrian-mason/wperf.git
cd wperf
git config --local user.name  "Adrian Mason"
git config --local user.email "258563901+adrian-mason@users.noreply.github.com"
git config --local core.hooksPath .githooks
hooks/preflight-identity.sh --strict
```

Missing any line in the secondary path is what PR #111 hit — the manual
clone fell through to the global `[--global] = ethercflow` baseline. The
preflight call is mandatory, not optional.

## 3. The Four Hook Layers

| Layer | Hook        | Script                        | What it refuses                          |
|-------|-------------|-------------------------------|------------------------------------------|
| L1    | pre-commit  | `hooks/check-git-identity.sh` | commit authored under any non-Adrian id  |
| L2    | commit-msg  | `hooks/dedup-trailers.sh`     | rewrites Co-Authored-By / duplicate SOB  |
| L3    | pre-push    | `hooks/check-git-identity.sh` | push / tag-push under any non-Adrian id  |
| L4    | git-wrapper | `hooks/git-wrapper.sh`        | write from external cwd (cadence-ext)    |

Under Q3=A all four layers are mandatory — no layer is "belt and
suspenders optional". L4 in particular is the only defence for the
`git -C <wperf>` invocation path from an unrelated cwd, which bypasses
`core.hooksPath` entirely unless the wrapper is sourced from shell init.

## 4. Six-Case Self-Test

Run before opening a PR that touches any commit-gate artifact:

```
hooks/self-test/run-all.sh
```

| Case | Purpose                                             | Layer |
|------|-----------------------------------------------------|-------|
| a    | Co-Authored-By stripped                             | L2    |
| b    | Signed-off-by deduplicated                          | L2    |
| c    | Clean message passes through unchanged              | L2    |
| d    | **[LOAD-BEARING]** commit under `wenbo.zhang@iomesh.com` refused | L1 |
| e    | push under `wenbo.zhang@iomesh.com` refused          | L3    |
| f    | write from external cwd via wrapper refused         | L4    |

**Case (d) is the primary regression guard for the PR #111 incident.**
It constructs a repo, sets `[--local] user.email = wenbo.zhang@iomesh.com`
explicitly, and asserts that the pre-commit hook exits non-zero. If
case (d) ever regresses, the gate has failed in exactly the shape of the
original incident — treat any case-(d) failure as a P0 revert signal.

Case (f) is non-overlapping with (d): case (d) blocks a locally-configured
wenbo commit via hook, case (f) blocks a cadence-external cwd write via
wrapper. Both must pass independently.

## 5. Appendix — Preflight BLOCK RCA

When `hooks/preflight-identity.sh --strict` or any of the four layers
rejects a write, the root cause is exactly one of the levels below. The
naming is canonical (L0 / L1(a) / L1(b) / L2) and is mirrored in
`feedback_git_identity_runtime_path.md` §A — changes here must stay
synchronised with that memory.

- **L0 — entry-point selection.** Which binary did the shell resolve
  for `git`? If a cadence-external shell invoked the real git directly,
  L4 (wrapper) does not fire and enforcement falls to L1–L3. If the
  wrapper is sourced, L0 hands off to L1(a).
- **L1(a) — abstract context logic.** Does the invocation resolve into
  a wperf worktree at all? The wrapper asks `git rev-parse --show-toplevel`
  against the effective `-C` / `GIT_DIR` target, and reads the top-level
  `Cargo.toml` for `name = "wperf"`. Non-wperf repos fall through.
- **L1(b) — cadence implementation.** When the context is wperf, what is
  the effective `[--local] user.name / user.email` for that worktree?
  The wrapper reads `git -C <repo> config --get user.{name,email}` and
  compares against Adrian's canonical identity.
- **L2 — hook enforcement backstop.** If the wrapper is absent or
  bypassed, the in-repo hooks (pre-commit / commit-msg / pre-push)
  enforce the same check against the identity git itself resolves.

**Oracle-mandatory BLOCK condition:** any change to this appendix that
introduces additional levels, renames the four above, or drops the
(a)/(b) sub-aspect of L1 must be rejected by review. The memory that
holds the canonical is `feedback_git_identity_runtime_path.md` §A; a
grep of this appendix against that §A must yield an exact match of the
four level names, or the gate has drifted from its documented RCA model.
