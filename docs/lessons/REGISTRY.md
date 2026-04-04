# Lessons Registry

Cumulative lessons promoted from phase signoffs. Each lesson is classified as:

- **Contract**: must be inherited by subsequent phases unless explicitly superseded
- **Enforcement**: automated via CI, tooling, or checklist
- **Observation**: historical context, not binding

## Active Contracts

| ID | Source | Title | Enforcement |
|----|--------|-------|-------------|
| L-001 | Gate 0 | [BPF userspace dependency baseline](L-001-bpf-dep-baseline.md) | CI `--features bpf`, Cargo.toml pin |
| L-002 | Gate 0 | [libbpf-rs 0.26 API patterns](L-002-libbpf-api-patterns.md) | Review checklist |
| L-003 | Phase 1 W1 | [Feature-gated code must have CI validation](L-003-feature-on-ci.md) | CI `cargo check --features bpf` |
| L-004 | Phase 1 W1 | [Dependency changes require baseline cross-check](L-004-dep-baseline-review.md) | PR checklist |

## Phase 1 Toolchain Baseline

Authoritative baseline for Phase 1. Deviations require ADR or explicit team sign-off.

| Component | Version | Source |
|-----------|---------|--------|
| Rust | 1.94.0 | `rust-toolchain.toml` |
| `libbpf-rs` | 0.26.x (resolves to 0.26.1) | Gate 0 signoff |
| `libbpf-sys` | 1.7.x | Gate 0 signoff |
| `clang` | 22.1 | Gate 0 signoff (enforced when #5 lands) |
| `libc` | 0.2.x (stable) | — |

## Carry-Forward Rules

1. Each Phase closeout must produce a "Promoted Lessons" section classifying new lessons as Contract / Enforcement / Observation.
2. Next Phase kickoff imports all active Contracts into its plan.
3. Machine-checkable constraints go into CI/tooling, not just docs.
4. Non-automatable lessons go into the review checklist.
