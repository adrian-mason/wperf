/* SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause) */
/* Copyright (c) 2022 Hengqi Chen */
/*
 * Vendored from bcc/libbpf-tools/compat.bpf.h
 * Upstream commit: 7f394c6d6775b9df68cac30b8147f9ab8a611ba7
 *                  "libbpf-tools: Add support for bpf_ringbuf"
 * Source: https://github.com/iovisor/bcc/blob/master/libbpf-tools/compat.bpf.h
 *
 * Adapted for wPerf (ADR-004 / ADR-002-supplement):
 *   - Map declarations moved to wperf.bpf.c (wperf-specific value type
 *     and buffer sizing; upstream uses generic MAX_EVENT_SIZE/RINGBUF_SIZE)
 *   - reserve_buf() and submit_buf() are kept verbatim from upstream,
 *     referencing the same map names ("events", "heap")
 *   - Drop counter added (not in upstream) — incremented on ringbuf
 *     reserve failure for user-space observability
 *
 * To update, run: scripts/sync-libbpf-compat.sh
 */

#ifndef __COMPAT_BPF_H
#define __COMPAT_BPF_H

#include <vmlinux.h>
#include <bpf/bpf_helpers.h>

/* Drop counter for ringbuf reserve failures (wperf extension).
 * Read by user-space at end of recording. */
extern __u64 drop_counter;

/*
 * reserve_buf() / submit_buf() — verbatim from upstream compat.bpf.h,
 * with wperf drop_counter instrumentation on the ringbuf path.
 *
 * Uses bpf_core_type_exists(struct bpf_ringbuf) as a CO-RE load-time
 * check to select ringbuf vs perfarray path. No runtime branching.
 */
static __always_inline void *reserve_buf(__u64 size)
{
	static const int zero = 0;

	if (bpf_core_type_exists(struct bpf_ringbuf)) {
		void *buf = bpf_ringbuf_reserve(&events, size, 0);

		if (!buf)
			__sync_fetch_and_add(&drop_counter, 1);
		return buf;
	}

	return bpf_map_lookup_elem(&heap, &zero);
}

static __always_inline long submit_buf(void *ctx, void *buf, __u64 size)
{
	if (bpf_core_type_exists(struct bpf_ringbuf)) {
		bpf_ringbuf_submit(buf, 0);
		return 0;
	}

	return bpf_perf_event_output(ctx, &events, BPF_F_CURRENT_CPU,
				     buf, size);
}

#endif /* __COMPAT_BPF_H */
