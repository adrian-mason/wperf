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
	WPERF_EVENT_SWITCH       = 1,
	WPERF_EVENT_WAKEUP       = 2,
	WPERF_EVENT_WAKEUP_NEW   = 3,
	WPERF_EVENT_EXIT         = 4,
	WPERF_EVENT_FUTEX_WAIT   = 5,
	WPERF_EVENT_IO_ISSUE     = 6,
	WPERF_EVENT_IO_COMPLETE  = 7,
};

/* Futex operation constants (from linux/futex.h).
 * Guarded: vmlinux.h (BTF dump) doesn't define these macros today,
 * but #ifndef is zero-cost insurance against future toolchain changes. */
#ifndef FUTEX_WAIT
#define FUTEX_WAIT              0
#endif
#ifndef FUTEX_LOCK_PI
#define FUTEX_LOCK_PI           6
#endif
#ifndef FUTEX_WAIT_BITSET
#define FUTEX_WAIT_BITSET       9
#endif
#ifndef FUTEX_WAIT_REQUEUE_PI
#define FUTEX_WAIT_REQUEUE_PI  11
#endif
#ifndef FUTEX_CMD_MASK
#define FUTEX_CMD_MASK         0x7f
#endif

/*
 * Futex event field mapping (reuses 40-byte wperf_event struct):
 *   prev_tid  → uaddr lower 32 bits
 *   next_tid  → uaddr upper 32 bits
 *   flags     → futex op (after FUTEX_CMD_MASK)
 *   prev_pid, next_pid, prev_state → unused (zero)
 */

/*
 * IO event field mapping (reuses 40-byte wperf_event struct, Phase 2b issue #38):
 *   timestamp_ns     → block_rq_issue / block_rq_complete ktime_get_boot_ns()
 *   pid              → submitter tgid (userspace PID; block_rq_complete reads from pending_io)
 *   tid              → submitter kernel tid (same source as pid)
 *   prev_tid         → sector lower 32 bits  (packed u64 sector)
 *   next_tid         → sector upper 32 bits  (packed u64 sector)
 *   prev_pid         → dev_t (u32)           — observability-only per ADR-009 / Challenger C11
 *   next_pid         → nr_sector (u32)       — request size in 512-byte sectors
 *   flags            → reserved (0; Path 1 sync-direct IO does not differentiate rw flags)
 *   prev_state, cpu  → unused / standard fill
 *
 * Discipline: correlate.rs Phase 2b MUST NOT dispatch on event.dev — load-bearing
 * comment required at the dev-read site. Retained for forward-compat with per-device
 * disk nodes (#115).
 */

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
