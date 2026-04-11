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

	if (self_tgid && (p->tgid == self_tgid ||
			  (__u32)(bpf_get_current_pid_tgid() >> 32) == self_tgid))
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

	/* Waker = current task. prev_tid/prev_pid encode waker identity
	 * per the event contract in src/format/event.rs. */
	__u64 pid_tgid = bpf_get_current_pid_tgid();
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

	if (self_tgid && (BPF_CORE_READ(p, tgid) == self_tgid ||
			  (__u32)(bpf_get_current_pid_tgid() >> 32) == self_tgid))
		return 0;

	e = reserve_buf(sizeof(*e));
	if (!e)
		return 0;

	fill_timestamp_and_cpu(e);
	e->event_type = WPERF_EVENT_WAKEUP;
	e->prev_state = 0;

	e->next_tid = BPF_CORE_READ(p, pid);
	e->next_pid = BPF_CORE_READ(p, tgid);

	/* Waker = current task. prev_tid/prev_pid encode waker identity
	 * per the event contract in src/format/event.rs. */
	__u64 pid_tgid = bpf_get_current_pid_tgid();
	e->pid = (__u32)(pid_tgid >> 32);
	e->tid = (__u32)pid_tgid;
	e->prev_tid = e->tid;
	e->prev_pid = e->pid;

	submit_buf(ctx, e, sizeof(*e));
	return 0;
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
