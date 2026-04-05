/* SPDX-License-Identifier: GPL-2.0 OR BSD-2-Clause */
/*
 * wperf.h — Shared definitions between BPF programs and user-space.
 *
 * This header defines the event structure and enums that must match
 * the Rust-side definitions in src/format/event.rs exactly.
 *
 * Layout: 40 bytes, naturally aligned (no __attribute__((packed))).
 * See ADR-004 and docs/design/final-design.md §2.4.
 */

#ifndef __WPERF_H
#define __WPERF_H

/* When included outside BPF context (e.g., standalone clang check),
 * pull in fixed-width types. In BPF programs, vmlinux.h provides these. */
#ifndef __VMLINUX_H__
#include <linux/types.h>
#endif

/* Event type discriminants — must match Rust EventType repr(u8). */
enum wperf_event_type {
	WPERF_EVENT_SWITCH     = 1,
	WPERF_EVENT_WAKEUP     = 2,
	WPERF_EVENT_WAKEUP_NEW = 3,
	WPERF_EVENT_EXIT       = 4,
};

/*
 * 40-byte event structure — Rust mirror: src/format/event.rs WperfEvent.
 *
 * Fields are ordered for natural alignment:
 *   u64 first, then u32s, then u16, then u8s, then u32 (flags fills padding).
 *
 * Must NOT use __attribute__((packed)) — BPF verifier rejects unaligned
 * access on kernels < 5.8.
 */
struct wperf_event {
	__u64 timestamp_ns;   /* offset  0: ktime_get_boot_ns() */
	__u32 pid;            /* offset  8: tgid (userspace PID) */
	__u32 tid;            /* offset 12: kernel tid (task->pid) */
	__u32 prev_tid;       /* offset 16: previous thread tid (sched_switch) */
	__u32 next_tid;       /* offset 20: next thread tid (sched_switch) */
	__u32 prev_pid;       /* offset 24: previous thread tgid */
	__u32 next_pid;       /* offset 28: next thread tgid */
	__u16 cpu;            /* offset 32: CPU core number */
	__u8  event_type;     /* offset 34: enum wperf_event_type */
	__u8  prev_state;     /* offset 35: TASK_* state before switch */
	__u32 flags;          /* offset 36: reserved (0 in Phase 1) */
};                            /* total: 40 bytes */

/* Compile-time size check (BPF programs should verify this). */
_Static_assert(sizeof(struct wperf_event) == 40,
	       "wperf_event must be exactly 40 bytes");

#endif /* __WPERF_H */
