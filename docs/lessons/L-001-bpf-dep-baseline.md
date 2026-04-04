---
id: L-001
source: Gate 0
applies_to: Phase 1+
status: active
---

# L-001: BPF Userspace Dependency Baseline

## What we learned

Gate 0 prototype validated `libbpf-rs 0.26.1` with `libbpf-sys 1.7.0`. The 0.26.x API
surface differs significantly from earlier versions (0.24 and below): skeleton open requires
`MaybeUninit<OpenObject>`, map operations use `MapHandle::create()` (not `MapBuilder`),
and trait imports (`SkelBuilder`, `OpenSkel`, `Skel`, `MapCore`) are explicit.

During Phase 1 W1, `libbpf-rs 0.24` was mistakenly selected without cross-referencing
Gate 0 signoff, causing a compile failure on the `bpf` feature path.

## Contract

- Phase 1 BPF userspace dependencies MUST be pinned to Gate 0 validated versions:
  - `libbpf-rs = "0.26"` (resolves to 0.26.1)
  - `libbpf-sys = "1.7"`
- Any deviation requires an explicit ADR or team sign-off with rationale.

## Verification

- `Cargo.toml` pins are the source of truth.
- CI runs `cargo check --features bpf` and `cargo clippy --all-targets --features bpf -- -D warnings`.
- PR review checklist requires baseline cross-check when BPF dependencies are added or modified.
