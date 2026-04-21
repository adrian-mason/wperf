// SPDX-License-Identifier: GPL-2.0 OR BSD-2-Clause
/*
 * wperf.bpf.c — Scheduler tracepoint probes for wPerf.
 *
 * Implements ADR-013: dual-variant sched_switch + sched_wakeup probes.
 *   - Primary: tp_btf/sched_switch + tp_btf/sched_wakeup (kernel 5.5+)
 *     BTF-typed pointers: direct field access for stable fields (pid, tgid).
 *   - Fallback: raw_tp/sched_switch + raw_tp/sched_wakeup (kernel 4.17+)
 *     Uses BPF_CORE_READ for all task_struct field access.
 *
 * Transport: ADR-004 dual-mode ringbuf/perfarray via compat.bpf.h
 * reserve_buf/submit_buf abstraction (vendored from libbpf-tools).
 * Map reconfiguration happens in user-space between open()/load().
 *
 * This file is compiled to wperf.bpf.o by libbpf-cargo's SkeletonBuilder.
 * It is NOT compiled as part of the normal cargo build — it requires
 * clang with BPF target and vmlinux.h.
 */

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#include "wperf.h"

/* --------------------------------------------------------------------------
 * Maps
 * --------------------------------------------------------------------------
 * Both ringbuf and perfarray maps are declared. User-space reconfigures
 * between open() and load() based on probe_ringbuf() result:
 *   - RingBuf mode: events is RINGBUF, heap is suppressed (set_autocreate=false)
 *   - PerfArray mode: events is reconfigured to PERF_EVENT_ARRAY, heap is active
 *
 * Map declarations are wperf-specific (typed value, sized buffers).
 * The upstream compat.bpf.h uses generic MAX_EVENT_SIZE/RINGBUF_SIZE;
 * we declare maps here and let compat.bpf.h reference them by name.
 */

/* Primary event output map. Declared as RINGBUF; user-space may reconfigure
 * to PERF_EVENT_ARRAY before load(). */
struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(max_entries, 16 * 1024 * 1024); /* 16 MiB default, overridable */
} events SEC(".maps");

/* Per-CPU staging area for perfarray path. Suppressed in ringbuf mode
 * via set_autocreate(false). */
struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct wperf_event);
} heap SEC(".maps");

/* BSS: drop counter for ringbuf path. Incremented by compat.bpf.h's
 * reserve_buf() when bpf_ringbuf_reserve returns NULL.
 * Read by user-space at end of recording. */
__u64 drop_counter = 0;

/* RODATA: enable futex tracing (Phase 2a). Defaults to false; user-space
 * sets to true between open() and load() when futex annotation is wanted. */
const volatile bool enable_futex_tracing = false;

/* BSS: wperf's own TGID, set by user-space before attach().
 * Probes skip events involving this TGID to prevent observer-effect
 * feedback loops (wperf's sleep/wake cycles triggering its own probes). */
__u32 self_tgid = 0;

/* --------------------------------------------------------------------------
 * Buffer abstraction: vendored from libbpf-tools compat.bpf.h
 * --------------------------------------------------------------------------
 * reserve_buf() / submit_buf() use CO-RE bpf_core_type_exists to select
 * ringbuf vs perfarray path at BPF load time. See compat.bpf.h for
 * implementation and provenance details.
 */
#include "compat.bpf.h"
#include "core_fixes.bpf.h"

/* --------------------------------------------------------------------------
 * Helper: fill common event fields
 * -------------------------------------------------------------------------- */

static __always_inline void fill_timestamp_and_cpu(struct wperf_event *e)
{
	e->timestamp_ns = bpf_ktime_get_ns();
	e->cpu = (__u16)bpf_get_smp_processor_id();
	e->flags = 0;
}

/* --------------------------------------------------------------------------
 * tp_btf variants (kernel 5.5+)
 *
 * BTF-typed task_struct pointers allow direct field access for stable
 * fields (pid, tgid). BPF_CORE_READ still used for __state (CO-RE
 * relocation needed — field name changed across kernel versions).
 * Controlled by set_autoload(): disabled on kernels without tp_btf support.
 * -------------------------------------------------------------------------- */

SEC("tp_btf/sched_switch")
int BPF_PROG(handle_sched_switch_btf,
	     bool preempt,
	     struct task_struct *prev,
	     struct task_struct *next)
{
	struct wperf_event *e;

	if (self_tgid && (prev->tgid == self_tgid || next->tgid == self_tgid))
		return 0;

	e = reserve_buf(sizeof(*e));
	if (!e)
		return 0;

	fill_timestamp_and_cpu(e);
	e->event_type = WPERF_EVENT_SWITCH;
	/* __state needs BPF_CORE_READ for CO-RE relocation (field name
	 * changed across kernel versions). pid/tgid are stable — direct
	 * access is safe with tp_btf's BTF-typed pointers. */
	e->prev_state = (__u8)get_task_state(prev);

	e->prev_tid = prev->pid;
	e->prev_pid = prev->tgid;
	e->next_tid = next->pid;
	e->next_pid = next->tgid;

	/* pid/tid from the BPF context (current task at switch time = prev). */
	__u64 pid_tgid = bpf_get_current_pid_tgid();
	e->pid = (__u32)(pid_tgid >> 32);
	e->tid = (__u32)pid_tgid;

	submit_buf(ctx, e, sizeof(*e));
	return 0;
}

SEC("tp_btf/sched_wakeup")
int BPF_PROG(handle_sched_wakeup_btf,
	     struct task_struct *p)
{
	struct wperf_event *e;
	__u64 pid_tgid = bpf_get_current_pid_tgid();

	if (self_tgid && (p->tgid == self_tgid ||
			  (__u32)(pid_tgid >> 32) == self_tgid))
		return 0;

	e = reserve_buf(sizeof(*e));
	if (!e)
		return 0;

	fill_timestamp_and_cpu(e);
	e->event_type = WPERF_EVENT_WAKEUP;
	e->prev_state = 0;

	/* Wakee — direct access via tp_btf BTF-typed pointer. */
	e->next_tid = p->pid;
	e->next_pid = p->tgid;

	e->pid = (__u32)(pid_tgid >> 32);
	e->tid = (__u32)pid_tgid;
	e->prev_tid = e->tid;
	e->prev_pid = e->pid;

	submit_buf(ctx, e, sizeof(*e));
	return 0;
}

/* --------------------------------------------------------------------------
 * raw_tp variants (kernel 4.17+)
 *
 * Fallback path using BPF_CORE_READ for all task_struct field access.
 * Enabled when tp_btf is not available (set_autoload on tp_btf = false).
 * -------------------------------------------------------------------------- */

SEC("raw_tp/sched_switch")
int BPF_PROG(handle_sched_switch_raw,
	     bool preempt,
	     struct task_struct *prev,
	     struct task_struct *next)
{
	struct wperf_event *e;

	if (self_tgid && (BPF_CORE_READ(prev, tgid) == self_tgid ||
			  BPF_CORE_READ(next, tgid) == self_tgid))
		return 0;

	e = reserve_buf(sizeof(*e));
	if (!e)
		return 0;

	fill_timestamp_and_cpu(e);
	e->event_type = WPERF_EVENT_SWITCH;
	e->prev_state = (__u8)get_task_state(prev);

	/* raw_tp: task_struct pointers require BPF_CORE_READ. */
	e->prev_tid = BPF_CORE_READ(prev, pid);
	e->prev_pid = BPF_CORE_READ(prev, tgid);
	e->next_tid = BPF_CORE_READ(next, pid);
	e->next_pid = BPF_CORE_READ(next, tgid);

	__u64 pid_tgid = bpf_get_current_pid_tgid();
	e->pid = (__u32)(pid_tgid >> 32);
	e->tid = (__u32)pid_tgid;

	submit_buf(ctx, e, sizeof(*e));
	return 0;
}

SEC("raw_tp/sched_wakeup")
int BPF_PROG(handle_sched_wakeup_raw,
	     struct task_struct *p)
{
	struct wperf_event *e;
	__u64 pid_tgid = bpf_get_current_pid_tgid();

	if (self_tgid && (BPF_CORE_READ(p, tgid) == self_tgid ||
			  (__u32)(pid_tgid >> 32) == self_tgid))
		return 0;

	e = reserve_buf(sizeof(*e));
	if (!e)
		return 0;

	fill_timestamp_and_cpu(e);
	e->event_type = WPERF_EVENT_WAKEUP;
	e->prev_state = 0;

	e->next_tid = BPF_CORE_READ(p, pid);
	e->next_pid = BPF_CORE_READ(p, tgid);

	e->pid = (__u32)(pid_tgid >> 32);
	e->tid = (__u32)pid_tgid;
	e->prev_tid = e->tid;
	e->prev_pid = e->pid;

	submit_buf(ctx, e, sizeof(*e));
	return 0;
}

/* --------------------------------------------------------------------------
 * sys_enter_futex tracepoint (Phase 2a — wait cause annotation)
 *
 * Standard tracepoint (not tp_btf/raw_tp). Stable ABI across all 4.18+
 * kernels — single handler, no dual-variant needed.
 * Gated by const volatile bool enable_futex_tracing.
 *
 * Captures only wait-side operations (FUTEX_WAIT, FUTEX_WAIT_BITSET,
 * FUTEX_LOCK_PI). Ignores FUTEX_WAKE and other non-blocking ops.
 *
 * Field mapping in wperf_event:
 *   prev_tid  = uaddr lower 32 bits
 *   next_tid  = uaddr upper 32 bits
 *   flags     = futex op (masked by FUTEX_CMD_MASK)
 * -------------------------------------------------------------------------- */

SEC("tracepoint/syscalls/sys_enter_futex")
int handle_sys_enter_futex(struct trace_event_raw_sys_enter *ctx)
{
	struct wperf_event *e;

	if (!enable_futex_tracing)
		return 0;

	__u64 pid_tgid = bpf_get_current_pid_tgid();
	__u32 tgid = (__u32)(pid_tgid >> 32);

	if (self_tgid && tgid == self_tgid)
		return 0;

	__u32 op = (__u32)ctx->args[1] & FUTEX_CMD_MASK;
	if (op != FUTEX_WAIT && op != FUTEX_WAIT_BITSET &&
	    op != FUTEX_LOCK_PI && op != FUTEX_WAIT_REQUEUE_PI)
		return 0;

	e = reserve_buf(sizeof(*e));
	if (!e)
		return 0;

	fill_timestamp_and_cpu(e);
	e->event_type = WPERF_EVENT_FUTEX_WAIT;
	e->pid = tgid;
	e->tid = (__u32)pid_tgid;

	__u64 uaddr = (__u64)ctx->args[0];
	e->prev_tid = (__u32)uaddr;
	e->next_tid = (__u32)(uaddr >> 32);

	e->prev_pid = 0;
	e->next_pid = 0;
	e->prev_state = 0;
	e->flags = op;

	submit_buf(ctx, e, sizeof(*e));
	return 0;
}

/* --------------------------------------------------------------------------
 * Block IO tracepoints (Phase 2b #38 P2b-01)
 *
 * Emit User→PseudoDisk (issue) + PseudoDisk→User (complete) synthetic edges
 * via tp_btf/raw_tp dual-attach (bcc biolatency pattern). Both variants
 * share handlers; autoload is gated by probe_tp_btf() result in record.rs.
 *
 * Kernel ABI history:
 *   - 5.4  : btf_trace_block_rq_issue ABSENT → raw_tp fallback only
 *   - 5.8  : btf_trace_block_rq_issue present, args = (__data, q, rq)
 *   - 5.11 : commit a54895fa dropped q → args = (__data, rq) [single-arg form]
 *   - 5.17 : rq_disk removed (f3fa33acca9f) → use q->disk (core_fixes get_disk)
 *
 * block_rq_complete signature stable across 5.8+: (__data, rq, error, nr_bytes).
 *
 * rodata targ_single: set by user-space from btf_vlen probe; selects ctx
 * slot holding `rq`. Single-arg (5.11+): ctx[0]. Dual-arg (pre-5.11): ctx[1].
 *
 * Attribution: submitter tgid/tid is captured at issue time (task context)
 * and replayed at complete time (softirq/IRQ context). Per ADR-009
 * / Challenger C11: edges attach to the submitting user task, not to the
 * interrupted task at completion.
 * -------------------------------------------------------------------------- */

#ifndef PF_KTHREAD
#define PF_KTHREAD 0x00200000
#endif

enum wperf_io_counter {
	IO_CNT_KTHREAD_SKIP     = 0,
	IO_CNT_PENDING_DROP     = 1,
	IO_CNT_ORPHAN_COMPLETE  = 2,
	IO_CNT_SUBMITTER_MISS   = 3,
	IO_CNT_MAX              = 4,
};

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, IO_CNT_MAX);
	__type(key, __u32);
	__type(value, __u64);
} io_counters SEC(".maps");

struct pending_io_val {
	__u32 submitter_tgid;
	__u32 submitter_tid;
};

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 16384);
	__type(key, __u64);
	__type(value, struct pending_io_val);
} pending_io SEC(".maps");

/* Set by user-space from btf_vlen("btf_trace_block_rq_issue"): true when
 * kernel has the post-5.11 single-arg form (rq at ctx[0]). Default true is
 * harmless for raw_tp path — raw_tp block_rq_issue ABI is (q, rq) pre-5.11
 * and (rq) post-5.11; only kernels ≥5.11 expose tp_btf here. */
const volatile bool targ_single = true;

static __always_inline void io_counter_inc(__u32 slot)
{
	__u64 *v = bpf_map_lookup_elem(&io_counters, &slot);
	if (v)
		__sync_fetch_and_add(v, 1);
}

static __always_inline bool is_kthread(struct task_struct *t)
{
	return (BPF_CORE_READ(t, flags) & PF_KTHREAD) != 0;
}

static __always_inline __u32 rq_dev(struct request *rq)
{
	struct gendisk *disk = get_disk(rq);
	if (!disk)
		return 0;
	__u32 major = BPF_CORE_READ(disk, major);
	__u32 first_minor = BPF_CORE_READ(disk, first_minor);
	return (major << 20) | first_minor;
}

static __always_inline int handle_block_rq_issue(void *ctx, struct request *rq)
{
	struct task_struct *cur = (struct task_struct *)bpf_get_current_task();
	struct wperf_event *e;

	if (is_kthread(cur)) {
		io_counter_inc(IO_CNT_KTHREAD_SKIP);
		return 0;
	}

	__u64 pid_tgid = bpf_get_current_pid_tgid();
	__u32 tgid = (__u32)(pid_tgid >> 32);
	__u32 tid  = (__u32)pid_tgid;

	if (self_tgid && tgid == self_tgid)
		return 0;

	__u64 key = (__u64)(unsigned long)rq;
	struct pending_io_val val = {
		.submitter_tgid = tgid,
		.submitter_tid  = tid,
	};
	if (bpf_map_update_elem(&pending_io, &key, &val, BPF_ANY) != 0) {
		io_counter_inc(IO_CNT_PENDING_DROP);
		return 0;
	}

	e = reserve_buf(sizeof(*e));
	if (!e)
		return 0;

	fill_timestamp_and_cpu(e);
	e->event_type = WPERF_EVENT_IO_ISSUE;
	e->pid = tgid;
	e->tid = tid;

	__u64 sector = BPF_CORE_READ(rq, __sector);
	e->prev_tid = (__u32)sector;
	e->next_tid = (__u32)(sector >> 32);

	e->prev_pid = rq_dev(rq);
	e->next_pid = BPF_CORE_READ(rq, __data_len) >> 9;
	e->prev_state = 0;

	submit_buf(ctx, e, sizeof(*e));
	return 0;
}

static __always_inline int handle_block_rq_complete(void *ctx, struct request *rq)
{
	__u64 key = (__u64)(unsigned long)rq;
	struct pending_io_val *val = bpf_map_lookup_elem(&pending_io, &key);
	if (!val) {
		io_counter_inc(IO_CNT_ORPHAN_COMPLETE);
		return 0;
	}

	__u64 cur_pid_tgid = bpf_get_current_pid_tgid();
	if ((__u32)cur_pid_tgid != val->submitter_tid)
		io_counter_inc(IO_CNT_SUBMITTER_MISS);

	struct wperf_event *e = reserve_buf(sizeof(*e));
	if (!e) {
		bpf_map_delete_elem(&pending_io, &key);
		return 0;
	}

	fill_timestamp_and_cpu(e);
	e->event_type = WPERF_EVENT_IO_COMPLETE;
	/* Attribute to submitter recorded at issue time — completion runs in
	 * softirq/IRQ context where current task is unrelated. ADR-009 §3. */
	e->pid = val->submitter_tgid;
	e->tid = val->submitter_tid;

	__u64 sector = BPF_CORE_READ(rq, __sector);
	e->prev_tid = (__u32)sector;
	e->next_tid = (__u32)(sector >> 32);

	e->prev_pid = rq_dev(rq);
	e->next_pid = BPF_CORE_READ(rq, __data_len) >> 9;
	e->prev_state = 0;

	submit_buf(ctx, e, sizeof(*e));
	bpf_map_delete_elem(&pending_io, &key);
	return 0;
}

SEC("tp_btf/block_rq_issue")
int BPF_PROG(handle_block_rq_issue_btf)
{
	struct request *rq = targ_single
		? (struct request *)ctx[0]
		: (struct request *)ctx[1];
	return handle_block_rq_issue(ctx, rq);
}

SEC("raw_tp/block_rq_issue")
int BPF_PROG(handle_block_rq_issue_raw)
{
	struct request *rq = targ_single
		? (struct request *)ctx[0]
		: (struct request *)ctx[1];
	return handle_block_rq_issue(ctx, rq);
}

SEC("tp_btf/block_rq_complete")
int BPF_PROG(handle_block_rq_complete_btf)
{
	return handle_block_rq_complete(ctx, (struct request *)ctx[0]);
}

SEC("raw_tp/block_rq_complete")
int BPF_PROG(handle_block_rq_complete_raw)
{
	return handle_block_rq_complete(ctx, (struct request *)ctx[0]);
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
