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

> **Terminology.** Throughout this document, `hooks/preflight-identity.sh`
> emits a **3-line / 6-field matrix** — one line per scope
> (`[--global]` / `[--local]` / `[effective]`), each line exposing
> `user.name` and `user.email` (3 × 2 = 6 fields). Any prior references
> to "3-echo" or "6-echo" refer to the same artefact under this name.

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
# Source the shell-init shim so L4 (git-wrapper) is active in this shell.
# Bash/zsh:   source hooks/wrapper.bash
# Fish:       source hooks/wrapper.fish
```

This is the supported path and the only one the reviewer team validates.
The `--strict` preflight run must exit 0 before the first commit. The
shell-init shim step is what makes L4 actually intercept `/usr/bin/git`
invocations; without it the wrapper exists on disk but nothing calls it.

### SECONDARY — manual clone (use only when cadence is unavailable)

```
git clone https://github.com/adrian-mason/wperf.git
cd wperf
git config --local user.name  "Adrian Mason"
git config --local user.email "258563901+adrian-mason@users.noreply.github.com"
git config --local core.hooksPath .githooks
hooks/preflight-identity.sh --strict
# Source the shell-init shim (same as PRIMARY).
# Bash/zsh:   source hooks/wrapper.bash
# Fish:       source hooks/wrapper.fish
```

Missing any line in the secondary path is what PR #111 hit — the manual
clone fell through to the global `[--global] = ethercflow` baseline. The
preflight call and the wrapper source step are mandatory, not optional.

## 3. The Four Hook Layers

> **Namespace note.** The labels `L1 / L2 / L3 / L4` in this section
> refer to the four hook / wrapper **enforcement implementation points**.
> The `L0 / L1(a) / L1(b) / L2` labels in Appendix §5 refer to the
> canonical **RCA framework** maintained in
> `feedback_git_identity_runtime_path.md` §A. The two label sets occupy
> different dimensions and must not be conflated — a regression in either
> is independent of the other.

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

**L1 ↔ L4 are mutual backstops.** The wrapper reads *stored* config
(`git -C <repo> config --get user.email`) and therefore does not see a
one-shot `-c user.email=…` override; the pre-commit hook runs in git's
own context and *does* see `-c` overrides. Conversely, subcommands the
wrapper enforces on (e.g. `notes`, `update-ref`, `replace`) do not fire
pre-commit. When either `core.hooksPath` is unset *or* the wrapper is
not sourced into the shell, the `-c` override path and the ref-write
path become unguarded. Preserve both layers.

> **Cross-domain isomorphism.** L4's "wrapper attach-point defined but
> workload bypasses it unless shell-init sources the shim" is the git-
> identity instantiation of `feedback_git_identity_runtime_path.md` §B
> row 3 — the same *runtime-path-connectivity* failure mode that the
> BPF-probe domain fixes with `losetup --direct-io=on`. Treating L4
> delivery as a runtime-path gate (not a spec-only claim) is therefore
> a project-wide discipline, not a wrapper-specific quirk.

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
- **L1(a) — abstract identity selection logic.** Does the invocation
  resolve into a wperf worktree at all? The wrapper asks
  `git rev-parse --show-toplevel` against the effective `-C` / `GIT_DIR`
  target, and reads the top-level `Cargo.toml` for `name = "wperf"`.
  Non-wperf repos fall through. The label matches
  `feedback_git_identity_runtime_path.md` §A top-level byte-for-byte;
  the §E RCA-tag variant ("abstract rule wrong or missing") is a
  shorthand, not a rename.
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
