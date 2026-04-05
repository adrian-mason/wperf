---
id: L-003
source: Phase 1 W1
applies_to: Phase 1+
status: active
---

# L-003: Feature-Gated Code Must Have CI Validation

## What we learned

PR #77 introduced an optional `bpf` feature with `cfg(feature = "bpf")` gated code.
The default build passed CI, but `cargo check --features bpf` failed due to a
nonexistent API (`MapBuilder`). This was only caught during manual reviewer verification,
not by CI.

## Contract

When a PR introduces or modifies a Cargo feature:
- CI MUST validate both the default build AND the feature-on build.
- At minimum: `cargo check --features <feature>` and `cargo clippy --all-targets --features <feature> -- -D warnings`.

## Verification

- `.github/workflows/ci.yml` includes `cargo check --features bpf` and
  `cargo clippy --all-targets --features bpf -- -D warnings` steps.
- PR review checklist: if a new feature is introduced, verify CI covers it.
