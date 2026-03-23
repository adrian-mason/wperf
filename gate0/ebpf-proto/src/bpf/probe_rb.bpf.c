// Gate 0: Ring buffer variant for buffer stress testing
// Same probes as probe.bpf.c but uses BPF_MAP_TYPE_RINGBUF instead of perf_event_array

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define EVENT_SWITCH 0
#define EVENT_WAKEUP 1

struct event {
    __u8  type;
    __u16 cpu;
    __u32 prev_pid;
    __u32 prev_tgid;
    __u32 next_pid;
    __u32 next_tgid;
    __u64 timestamp_ns;
    __u32 prev_state;
} __attribute__((packed));

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 32 * 1024 * 1024); // 32MB default, overridden from userspace
} events SEC(".maps");

// Drop counter — incremented when ringbuf is full
__u64 drop_count = 0;

SEC("raw_tp/sched_switch")
int handle_switch(struct bpf_raw_tracepoint_args *ctx)
{
    struct task_struct *prev = (struct task_struct *)ctx->args[1];
    struct task_struct *next = (struct task_struct *)ctx->args[2];

    struct event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
    if (!e) {
        __sync_fetch_and_add(&drop_count, 1);
        return 0;
    }

    e->type = EVENT_SWITCH;
    e->cpu = bpf_get_smp_processor_id();
    e->prev_pid  = BPF_CORE_READ(prev, pid);
    e->prev_tgid = BPF_CORE_READ(prev, tgid);
    e->next_pid  = BPF_CORE_READ(next, pid);
    e->next_tgid = BPF_CORE_READ(next, tgid);
    e->timestamp_ns = bpf_ktime_get_ns();
    e->prev_state = BPF_CORE_READ(prev, __state);

    bpf_ringbuf_submit(e, 0);
    return 0;
}

SEC("raw_tp/sched_wakeup")
int handle_wakeup(struct bpf_raw_tracepoint_args *ctx)
{
    struct task_struct *p = (struct task_struct *)ctx->args[0];

    struct event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
    if (!e) {
        __sync_fetch_and_add(&drop_count, 1);
        return 0;
    }

    e->type = EVENT_WAKEUP;
    e->cpu = bpf_get_smp_processor_id();
    e->prev_pid  = bpf_get_current_pid_tgid() & 0xFFFFFFFF;
    e->prev_tgid = bpf_get_current_pid_tgid() >> 32;
    e->next_pid  = BPF_CORE_READ(p, pid);
    e->next_tgid = BPF_CORE_READ(p, tgid);
    e->timestamp_ns = bpf_ktime_get_ns();
    e->prev_state = 0;

    bpf_ringbuf_submit(e, 0);
    return 0;
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
