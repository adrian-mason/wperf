---
id: L-004
source: Phase 1 W1
applies_to: Phase 1+
status: active
---

# L-004: Dependency Changes Require Baseline Cross-Check

## What we learned

Gate 0 signoff documented `libbpf-rs 0.26.1` as the validated baseline, but this was
recorded as a historical observation, not an authoritative contract. When PR #77
introduced `libbpf-rs 0.24`, neither the author nor three reviewers caught the
mismatch — review focused on API correctness and code logic, not version alignment.

## Contract

When a PR adds or modifies a direct dependency (especially BPF/toolchain/FFI crates):
1. Author MUST cross-check the version against the toolchain baseline in `docs/lessons/REGISTRY.md`.
2. If the version deviates from baseline, the PR description MUST include a rationale.
3. Reviewer MUST verify dependency versions against baseline as a checklist item.

## Verification

- PR template includes a dependency baseline checklist item.
- Review checklist: "If this PR adds/modifies dependencies, are versions aligned with
  the baseline in `docs/lessons/REGISTRY.md`?"
