# ADR-004-supplement: Event Transport — User-Space Handling Details

- **Status:** Accepted
- **Date:** 2026-03-18
- **Supplements:** [ADR-004: Event Transport Strategy](ADR-004.md)

## Purpose

ADR-004 established the single-ELF CO-RE dual-mode transport strategy, focusing primarily on the BPF-side `reserve_buf()`/`submit_buf()` abstraction and the user-space `EventTransport` trait concept. This supplement documents the concrete user-space implementation patterns from libbpf-tools `compat.c`, providing the engineering specification needed for wPerf's Rust implementation.

## User-Space Dual-Mode Lifecycle

### Phase 1: Probe and Reconfigure (Between `open()` and `load()`)

The full reconfiguration sequence, derived from `compat.c:bpf_buffer__new()`:

```
skel = wperf_bpf::open()?;

let has_ringbuf = probe_ringbuf();   // attempt bpf_map_create(BPF_MAP_TYPE_RINGBUF)

if has_ringbuf {
    // Suppress unused percpu-array staging map
    skel.maps.heap.set_autocreate(false)?;
} else {
    // Reconfigure events map from RINGBUF → PERF_EVENT_ARRAY
    skel.maps.events.set_type(MapType::PerfEventArray)?;
    skel.maps.events.set_key_size(size_of::<i32>() as u32)?;
    skel.maps.events.set_value_size(size_of::<i32>() as u32)?;
}

skel.load()?;
skel.attach()?;
```

Key details not in ADR-004:
- **PerfEventArray reconfiguration requires setting key_size and value_size** to `sizeof(int)` — the BPF map definition for RINGBUF has these as 0, but PERF_EVENT_ARRAY requires them
- **`set_autocreate(false)`** prevents the kernel from attempting to create the map type. On pre-5.8 kernels, `BPF_MAP_TYPE_RINGBUF` is an unknown type — without `set_autocreate(false)` on the ringbuf map (or reconfiguring it), the load would fail with `EINVAL`

### Phase 2: Open Consumer

**Ringbuf path:**
```rust
let rb = RingBufferBuilder::new()
    .add(&skel.maps.events, callback)?
    .build()?;
```

Single file descriptor. Global FIFO ordering guaranteed. Callback signature: `fn(data: &[u8]) -> i32` — no CPU index parameter (CPU must be embedded in event data, which wPerf's `wperf_event` struct already includes via the `cpu` field).

**Perfarray path:**
```rust
let pb = PerfBufferBuilder::new(&skel.maps.events)
    .sample_cb(sample_callback)
    .lost_cb(lost_callback)
    .pages(PERF_BUFFER_PAGES)  // per-CPU buffer size
    .build()?;
```

N file descriptors (one per CPU). Per-CPU ordering only. The `lost_callback` fires when a per-CPU buffer overflows — this is the perfarray's built-in drop notification (unlike ringbuf, which requires explicit BPF-side `drop_counter` handling).

### Phase 3: Poll Loop

Both paths converge to a unified poll interface:

```rust
// Unified trait
trait EventTransport {
    fn poll(&mut self, timeout_ms: i32) -> Result<()>;
}

// RingBuf implementation
impl EventTransport for RingBufTransport {
    fn poll(&mut self, timeout_ms: i32) -> Result<()> {
        self.rb.poll(Duration::from_millis(timeout_ms as u64))?;
        Ok(())
    }
}

// PerfBuffer implementation
impl EventTransport for PerfBufTransport {
    fn poll(&mut self, timeout_ms: i32) -> Result<()> {
        self.pb.poll(Duration::from_millis(timeout_ms as u64))?;
        Ok(())
    }
}
```

The libbpf-tools `bpf_buffer__poll()` (compat.c:89-99) dispatches to `perf_buffer__poll()` or `ring_buffer__poll()` based on the stored type. The Rust implementation uses trait dispatch for the same purpose.

## Drop Detection Differences

| Aspect | Ringbuf | Perfarray |
|--------|---------|-----------|
| **Drop notification** | No automatic callback. BPF code must increment a `drop_counter` when `bpf_ringbuf_reserve()` returns NULL | Built-in `lost_cb` callback fires with the count of lost samples |
| **Drop granularity** | Per-event (each failed reserve is one drop) | Per-batch (lost_cb reports count of events lost in a single overflow) |
| **wPerf implementation** | BPF-side: `if (!buf) { __sync_fetch_and_add(&drop_counter, 1); return 0; }` | Rust-side: accumulate `lost_cb` counts into a metric |
| **User visibility** | Read `drop_counter` from BSS section at end of recording | Sum of all `lost_cb` invocations during recording |

Both paths must expose drop counts in the recording metadata for the coverage metrics specified in ADR-012.

## Ringbuf Notification Tuning

The Nakryiko blog ("BPF ring buffer") identifies in-kernel notification signaling as the primary ringbuf overhead on high-throughput workloads. libbpf's default ringbuf behavior is adaptive notification, but two flags are available:

| Flag | Effect | Use case |
|------|--------|----------|
| `BPF_RB_NO_WAKEUP` | Suppress epoll notification for this event | High-frequency events where polling is time-based |
| `BPF_RB_FORCE_WAKEUP` | Force epoll notification regardless of adaptive heuristic | Ensuring timely delivery of critical events |

For wPerf's `sched_switch` events (potentially 100K+/sec on busy systems), a hybrid strategy is possible:

```c
// In BPF submit_buf():
bpf_ringbuf_submit(buf, (count % 64 == 0) ? BPF_RB_FORCE_WAKEUP : BPF_RB_NO_WAKEUP);
```

This batches notifications to once per 64 events, reducing kernel-to-userspace signaling overhead. However, this optimization should be deferred to Phase 2 (performance tuning) after baseline measurements confirm that notification overhead is actually a bottleneck.

## Ringbuf Sizing Constraints

- `max_entries` must be a power of 2 and a multiple of kernel page size (typically 4096)
- libbpf 1.0+ auto-rounds to the proper multiple
- The size can be overridden from user-space via `bpf_map__set_max_entries()` between `open()` and `load()`
- wPerf should expose this as a `--buffer-size` CLI flag for `wperf record`, with a sensible default (e.g., 16MB)

## Consequences

1. **EventTransport Rust trait**: Must implement the `new()` → `open()` → `poll()` lifecycle with probe-based mode selection at construction time. The perfarray path's `PerfBufferBuilder` requires explicit page count configuration.

2. **Drop metric unification**: Both transport paths must produce a single `drop_count` metric, despite different detection mechanisms (BPF-side counter vs user-side callback). The recording metadata format must accommodate this.

3. **Notification tuning**: Deferred to Phase 2. The baseline implementation uses default adaptive notification. If profiling shows notification overhead exceeding the <3% CPU target, the `BPF_RB_NO_WAKEUP` batching strategy can be applied.

4. **Buffer sizing**: The `wperf record` CLI should accept `--buffer-size` with power-of-2 validation, passed to `bpf_map__set_max_entries()` before load.
