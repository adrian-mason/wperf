---
id: L-002
source: Gate 0
applies_to: Phase 1+
status: active
---

# L-002: libbpf-rs 0.26 API Patterns

## What we learned

Gate 0 prototype documented several libbpf-rs 0.26.1 API patterns that differ from
documentation examples or older versions:

1. **Skeleton open**: requires `MaybeUninit<OpenObject>` — not a simple `::open()`.
2. **Trait imports**: `SkelBuilder`, `OpenSkel`, `Skel`, `MapCore` must be explicitly imported.
3. **Packed struct fields**: copy field to local variable before formatting (`format!`),
   otherwise Rust's reference rules conflict with packed repr.
4. **`RingBufferBuilder`**: requires mutable binding before `.build()`.
5. **Map creation**: use `MapHandle::create()`, not `MapBuilder` (which does not exist).

## Contract

When writing code against `libbpf-rs 0.26.x`:
- Follow the patterns documented in `docs/gate0/ebpf-prototype.md` §5-6.
- Do not assume API compatibility with older versions or generic examples.

## Verification

- Code review: reviewer should cross-check libbpf-rs API usage against this lesson
  and Gate 0 prototype notes when reviewing BPF-facing code.
- `cargo check --features bpf` catches compile-time mismatches.
