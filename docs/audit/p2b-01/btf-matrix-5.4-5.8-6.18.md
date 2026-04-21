# P2b-01 BTF matrix — kernel 5.4 / 5.8 / 6.18 datapoints

**Captured:** 2026-04-21 (Maestro)
**Sources:**
- 6.18 — local `/sys/kernel/btf/vmlinux` (`6.18.22-1-lts`)
- 5.4  — btfhub-archive `ubuntu/20.04/x86_64/5.4.0-91-generic.btf`
- 5.8  — btfhub-archive `ubuntu/20.04/x86_64/5.8.0-33-generic.btf`
**Tool:** `bpftool btf dump file <path> format raw`

## Tracepoint handler BTF shape

### `btf_trace_block_rq_issue`

```
[27601] TYPEDEF 'btf_trace_block_rq_issue' type_id=13837
[13837] PTR '(anon)' type_id=13836
[13836] FUNC_PROTO '(anon)' ret_type_id=0 vlen=2
        '(anon)' type_id=109   -> void *  (__data)
        '(anon)' type_id=2610  -> struct request *
```

**ABI:** `void handler(void *__data, struct request *rq)` — **post-5.11 form** (no `struct request_queue *q`).

### `btf_trace_block_rq_complete`

```
[27595] TYPEDEF 'btf_trace_block_rq_complete' type_id=13839
[13839] PTR '(anon)' type_id=13838
[13838] FUNC_PROTO '(anon)' ret_type_id=0 vlen=4
        '(anon)' type_id=109   -> void *  (__data)
        '(anon)' type_id=2610  -> struct request *
        '(anon)' type_id=2058  -> blk_status_t  (error)
        '(anon)' type_id=9     -> unsigned int  (nr_bytes)
```

**ABI:** `void handler(void *__data, struct request *rq, blk_status_t error, unsigned int nr_bytes)` — 4-arg, stable across the post-5.11 range.

## Kernel 5.8 — `btf_trace_block_rq_issue` present, 3-arg ABI

```
[54659] TYPEDEF 'btf_trace_block_rq_issue' type_id=26557
[26557] PTR '(anon)' type_id=26556
[26556] FUNC_PROTO '(anon)' ret_type_id=0 vlen=3
        '(anon)' type_id=93    -> void *             (__data)
        '(anon)' type_id=925   -> struct request_queue *  (q)
        '(anon)' type_id=2592  -> struct request *

[54655] TYPEDEF 'btf_trace_block_rq_complete' type_id=26559
[26559] PTR '(anon)' type_id=26558
[26558] FUNC_PROTO '(anon)' ret_type_id=0 vlen=4
        '(anon)' type_id=93    -> void *             (__data)
        '(anon)' type_id=2592  -> struct request *   (rq)
        '(anon)' type_id=21    -> int                (error)
        '(anon)' type_id=9     -> unsigned int       (nr_bytes)
```

**ABI:** `void handler(void *__data, struct request_queue *q, struct request *rq)` for issue — **pre-fork form with `q` as 2nd arg**.
Complete handler is 4-arg, same shape as 6.18.

`btf_trace_block_rq_insert` and `btf_trace_block_rq_requeue` share the same `type_id=26557` → same 3-arg proto.

## Kernel 5.4 — `btf_trace_block_rq_issue` ABSENT

```
$ bpftool btf dump file 5.4.0-91-generic.btf format raw | grep btf_trace_block_rq
(no output — no btf_trace_block_rq_* typedefs exist in 5.4 vmlinux BTF)
```

5.4 vmlinux BTF contains `trace_event_raw_block_rq_*` structs and `__bpf_trace_block_rq_*` functions (the legacy perf-event tracepoint path) but **no `btf_trace_*` typedefs**. These typedefs were added mainline in the same series as tp_btf program support (landed ~5.5, generalized ~5.8).

**Implication:** tp_btf programs targeting `block_rq_issue` / `block_rq_complete` / `block_rq_insert` **cannot load on 5.4 at all** — the verifier fails type resolution before ever reaching the handler body.

## Kernel matrix summary

| Kernel | `btf_trace_block_rq_issue` | issue ABI | complete ABI | tp_btf viable? |
|--------|----------------------------|-----------|--------------|----------------|
| 5.4.0-91-generic  | **absent**               | N/A       | N/A          | **NO**         |
| 5.8.0-33-generic  | present                  | 3-arg (`__data, q, rq`) | 4-arg | yes (pre-fork) |
| 6.18.22-1-lts     | present                  | 2-arg (`__data, rq`)    | 4-arg | yes (post-fork) |

Two independent compatibility axes, not one:
1. **tp_btf presence** — 5.4 fails before ABI even matters.
2. **issue handler arg shape** — 3-arg (pre-5.11 / pre-5.10.137 stable) vs 2-arg (post-patch). BCC `biosnoop.bpf.c` handles this with `LINUX_KERNEL_VERSION` + `ctx[0]` / `ctx[1]` indexing inside a single `tp_btf/block_rq_issue` program.

## Implementation-path options for commit-2

| Option | 5.4 ok? | 5.8–5.10 ok? | 5.11+ ok? | Programs | Complexity |
|--------|---------|--------------|-----------|----------|------------|
| A. classic `tracepoint/block/block_rq_issue` (format-file) | YES | YES | YES | 6 | low — ABI-stable across all kernels, reads fields via `bpf_probe_read_kernel` or format-offsets |
| B. tp_btf 2a+2b split (pre/post-fork) | NO | YES (2a) | YES (2b) | 8 | medium — but drops 5.4 |
| C. tp_btf single-program + `ctx[0]` / `ctx[1]` runtime branch | NO | YES | YES | 6 | medium — drops 5.4, but single code path |

**Recommendation:** option A (classic tracepoint) if 5.4 support is a requirement.
`get_disk()` CO-RE helper (just vendored into `src/bpf/core_fixes.bpf.h`) works transparently under both classic and tp_btf attach styles, so the rq_disk/q→disk rename is independently handled regardless of option.

## Next

- Decide on option A / B / C based on 5.4 support requirement.
- If option A: commit-2 stays single path, no 2a/2b split. 7-gate self-checklist proceeds as planned.
- If dropping 5.4: pick B or C based on whether the simpler-single-program (C) or stricter-typed-signature (B) reads better.
