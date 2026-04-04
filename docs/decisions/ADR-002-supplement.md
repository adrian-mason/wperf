# ADR-002-supplement: Kernel Compatibility — libbpf-tools Empirical Alignment

- **Status:** Accepted
- **Date:** 2026-03-18
- **Supplements:** [ADR-002: Dynamic Feature Probing](ADR-002.md)

## Purpose

ADR-002 established dynamic per-feature probing as the kernel compatibility strategy, citing the bcc/libbpf-tools codebase as the production-validated reference. This supplement provides the empirical evidence from line-by-line analysis of the libbpf-tools source code, correcting two inaccuracies in the original ADR and documenting implementation patterns that the wPerf codebase must follow.

## Finding 1: ringbuf/perfbuf Dual-Mode — Full Stack Confirmed (Q1)

### BPF-Side Abstraction (`compat.bpf.h`)

The dual-mode transport abstraction consists of two maps and two inline functions:

**Map declarations** (compat.bpf.h:13-23):

```c
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __uint(key_size, sizeof(__u32));
    __uint(value_size, MAX_EVENT_SIZE);
} heap SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, RINGBUF_SIZE);
} events SEC(".maps");
```

Both maps are always declared. The user-space loader decides which to use at runtime.

**`reserve_buf()`** (compat.bpf.h:25-33) — uses `bpf_core_type_exists(struct bpf_ringbuf)` as a CO-RE compile-time check. If ringbuf exists in the running kernel's BTF, calls `bpf_ringbuf_reserve(&events, size, 0)`. Otherwise, falls back to `bpf_map_lookup_elem(&heap, &zero)` to get a percpu staging slot.

**`submit_buf()`** (compat.bpf.h:35-43) — mirrors the reserve pattern. Ringbuf path calls `bpf_ringbuf_submit(buf, 0)` (zero-copy). Perfarray path calls `bpf_perf_event_output(ctx, &events, BPF_F_CURRENT_CPU, buf, size)` (copy from staging slot).

### User-Space Abstraction (`compat.c`)

The user-space side implements a `bpf_buffer` abstraction that ADR-002 and ADR-004 referenced only at the conceptual level. The actual implementation:

**`bpf_buffer__new(events, heap)`** (compat.c:12-58):
1. Calls `probe_ringbuf()` to test kernel capability
2. If ringbuf available: `bpf_map__set_autocreate(heap, false)` — suppresses kernel creation of the unused percpu-array map
3. If ringbuf unavailable: `bpf_map__set_type(events, BPF_MAP_TYPE_PERF_EVENT_ARRAY)` — reconfigures the events map type, plus sets key_size and value_size to `sizeof(int)`

**`probe_ringbuf()`** (trace_helpers.c:1239-1249) — attempts `bpf_map_create(BPF_MAP_TYPE_RINGBUF, NULL, 0, 0, getpagesize(), NULL)`. Success means ringbuf is available. The map fd is immediately closed — this is purely a capability probe.

**`bpf_buffer__open()`** (compat.c:60-87) — wraps libbpf's `perf_buffer__new()` or `ring_buffer__new()` depending on detected mode.

**`bpf_buffer__poll()`** (compat.c:89-99) — unified polling dispatches to `perf_buffer__poll()` or `ring_buffer__poll()`.

### Alignment with ADR-004

ADR-004's description of the dual-mode mechanism is accurate in architecture but incomplete in user-space detail. The `EventTransport` trait described in ADR-004 maps directly to the `bpf_buffer` C abstraction. The Rust implementation should follow the same structure:

| compat.c function | wPerf Rust equivalent |
|---|---|
| `bpf_buffer__new()` | `EventTransport::new()` with probe + map reconfiguration |
| `bpf_buffer__open()` | `EventTransport::open()` creating `RingBuffer` or `PerfBuffer` |
| `bpf_buffer__poll()` | `EventTransport::poll()` dispatching to the active transport |

## Finding 2: Non-BTF Fallback — Correction Required (Q2)

### Correction: "Hardcoded Offsets" Is Inaccurate

ADR-002's probe matrix states:

> BTF (vmlinux) | Check `/sys/kernel/btf/vmlinux` | Embedded BTF fallback; if still fails, **hardcoded offsets**

**This is incorrect.** Analysis of the entire libbpf-tools codebase (all `.bpf.c` files) found:
- **Zero** instances of `offsetof()` or hardcoded struct member offsets
- **Zero** instances of `BPF_PROBE_READ` (non-CO-RE macro)
- **Exclusive** use of CO-RE relocations via `BPF_CORE_READ()` and `bpf_core_field_exists()`

The actual fallback chain is:

1. **System BTF**: Check `/sys/kernel/btf/vmlinux` — modern kernels (5.4+ with `CONFIG_DEBUG_INFO_BTF`)
2. **Embedded BTF**: `ensure_core_btf()` (btf_helpers.c:165-236) extracts a compressed BTF archive (`min_core_btfs.tar.gz`) compiled into the binary, containing minimal BTF for known kernel versions
3. **Failure**: If neither system nor embedded BTF is available, return `-EOPNOTSUPP` — the tool refuses to run

There is no hardcoded-offset fallback tier. CO-RE relocations are the only mechanism; they require BTF (either system or embedded) to function.

### `core_fixes.bpf.h` Patterns

The 327-line `core_fixes.bpf.h` provides four CO-RE compatibility patterns, all requiring BTF:

**Pattern 1 — Field rename** (e.g., `task_struct.state` → `task_struct.__state`):
```c
struct task_struct___o { volatile long int state; } __attribute__((preserve_access_index));
struct task_struct___x { unsigned int __state; }    __attribute__((preserve_access_index));

static __always_inline __s64 get_task_state(void *task) {
    struct task_struct___x *t = task;
    if (bpf_core_field_exists(t->__state))
        return BPF_CORE_READ(t, __state);
    return BPF_CORE_READ((struct task_struct___o *)task, state);
}
```

**Pattern 2 — Structural change** (e.g., `bio.bi_disk` → `bio.bi_bdev->bd_disk`): new path chases an extra pointer.

**Pattern 3 — Type rename** (e.g., `trace_event_raw_block_rq_complete` → `trace_event_raw_block_rq_completion`): uses `bpf_core_type_exists()`.

**Pattern 4 — Bitfield to flags migration** (e.g., `inet_sock` individual bitfields → packed `inet_flags`): uses `bpf_core_field_exists()` + `BPF_CORE_READ_BITFIELD_PROBED()` for old layout, manual bit extraction for new.

### Corrected Probe Matrix Row

| Feature | Probe Method | Degradation |
|---------|-------------|-------------|
| **BTF (vmlinux)** | Check `/sys/kernel/btf/vmlinux` | **Phase 1: refuse to run (`EOPNOTSUPP`).** Minimum supported: RHEL 8.2+ / Rocky 8.4+ (`CONFIG_DEBUG_INFO_BTF=y`). Embedded BTF fallback deferred. |

### Implication for wPerf

wPerf must either:
- **(a)** Ship embedded BTF for target kernel versions (RHEL 8.x, Ubuntu 20.04/22.04 LTS, etc.) using the `min_core_btfs` pattern, or
- **(b)** Accept that kernels without BTF (pre-5.4 without backports, custom kernels without `CONFIG_DEBUG_INFO_BTF`) are unsupported

**Phase 1 decision (2026-04-04): Option (b) adopted.** Minimum supported kernel: RHEL 8.2+ / Rocky 8.4+ (kernel 4.18.0-193+, `CONFIG_DEBUG_INFO_BTF=y`). RHEL 8.0-8.1 (EOL, no BTF) are unsupported. Option (a) deferred to future evaluation if demand arises.

## Finding 3: cgroupv2 Filtering — Implementation Pattern (Q3)

### BPF-Side Pattern (13+ Tools)

All cgroup-filtering tools follow an identical pattern:

**Map declaration:**
```c
struct {
    __uint(type, BPF_MAP_TYPE_CGROUP_ARRAY);
    __type(key, u32);
    __type(value, u32);
    __uint(max_entries, 1);
} cgroup_map SEC(".maps");
```

**Filter check** (first lines of every BPF program entry):
```c
if (filter_cg && !bpf_current_task_under_cgroup(&cgroup_map, 0))
    return 0;
```

The `filter_cg` variable is a `const volatile bool` set from user-space before load.

### User-Space Pattern

From biosnoop.c (representative of all 13+ tools):

```c
if (env.cg) {
    idx = 0;
    cg_map_fd = bpf_map__fd(obj->maps.cgroup_map);
    cgfd = open(env.cgroupspath, O_RDONLY);    // Open cgroupv2 path
    bpf_map_update_elem(cg_map_fd, &idx, &cgfd, BPF_ANY);  // Store FD in map
}
```

### Key Findings

1. **cgroupv2 only** — all tools reference `/sys/fs/cgroup/unified/` paths. The `bpf_current_task_under_cgroup()` BPF helper (kernel 5.0+) works with cgroupv2 file descriptors
2. **No runtime cgroup version detection** — the implementation assumes cgroupv2. If passed an invalid path, `open()` fails gracefully
3. **Single-entry map** — `BPF_MAP_TYPE_CGROUP_ARRAY` with `max_entries=1` stores exactly one target cgroup FD
4. **No cgroupv1 support** — no tool implements cgroupv1 filtering

### Alignment with ADR-002

ADR-002's probe matrix states:

> cgroupv2 | Check `/sys/fs/cgroup/cgroup.controllers` | Disable cgroup filtering

This is correct. The implementation should:
1. Check cgroupv2 availability via `/sys/fs/cgroup/cgroup.controllers`
2. If available and user passes `--cgroup <path>`: open the cgroup path, store FD in `BPF_MAP_TYPE_CGROUP_ARRAY`, set `filter_cg = true`
3. If unavailable and user passes `--cgroup`: error out with a clear message
4. If unavailable and user does not pass `--cgroup`: set `filter_cg = false`, disable cgroup map via `set_autocreate(false)`

## Consequences

1. **ADR-002 probe matrix correction**: The BTF row has been updated to "refuse to run (`EOPNOTSUPP`)" as the final fallback, propagated to `final-design.md` section 1.3.

2. **Embedded BTF scope decision (resolved 2026-04-04)**: Option (b) adopted — BTF is a hard requirement for Phase 1. Minimum supported: RHEL 8.2+ / Rocky 8.4+ (`CONFIG_DEBUG_INFO_BTF=y`). Embedded BTF (`min_core_btfs`) deferred to future evaluation.

3. **User-space transport abstraction**: Uses `enum BpfBuffer { Ring(RingBufTransport), Perf(PerfBufTransport) }` — closed-variant enum dispatch instead of `Box<dyn Trait>`. Rationale: no heap indirection, no vtable, closed variant set (only Ring/Perf), compiler can inline through match. API: `poll(&mut self, timeout, f: impl FnMut(&WperfEvent)) -> Result<usize>`, `drain(&mut self, f: impl FnMut(&WperfEvent))` (perf reorder buffer drain; no-op for Ring), `drop_count(&self) -> u64`. Callback-based to avoid heap allocation on the hot path — libbpf-rs is already callback-based, and ringbuf events can be referenced in-place from mmap'd memory. The `bpf_buffer` pattern from `compat.c` is the reference design; BPF side vendors `compat.bpf.h` directly, userspace implements equivalent logic in Rust.

4. **Cgroup filtering architecture**: Follows the established `BPF_MAP_TYPE_CGROUP_ARRAY` + `bpf_current_task_under_cgroup()` pattern. cgroupv1 is explicitly not supported.
