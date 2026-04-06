/* SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause) */
/* Copyright (c) 2021 Hengqi Chen */
/*
 * Vendored from bcc/libbpf-tools/core_fixes.bpf.h
 * Upstream commit: 82ad428c40cb270fda6c0de5a9914705c94dd4c7
 * Source: https://github.com/iovisor/bcc/blob/master/libbpf-tools/core_fixes.bpf.h
 *
 * Only the task_struct state/\__state rename fix is included here.
 * Other CO-RE fixes from upstream are not needed by wPerf.
 *
 * To update, run: scripts/sync-libbpf-compat.sh
 */

#ifndef __CORE_FIXES_BPF_H
#define __CORE_FIXES_BPF_H

#include <vmlinux.h>
#include <bpf/bpf_core_read.h>

/**
 * commit 2f064a59a1 ("sched: Change task_struct::state") changes
 * the name of task_struct::state to task_struct::__state
 * see:
 *     https://github.com/torvalds/linux/commit/2f064a59a1
 */
struct task_struct___o {
	volatile long int state;
} __attribute__((preserve_access_index));

struct task_struct___x {
	unsigned int __state;
} __attribute__((preserve_access_index));

static __always_inline __s64 get_task_state(void *task)
{
	struct task_struct___x *t = task;

	if (bpf_core_field_exists(t->__state))
		return BPF_CORE_READ(t, __state);
	return BPF_CORE_READ((struct task_struct___o *)task, state);
}

#endif /* __CORE_FIXES_BPF_H */
