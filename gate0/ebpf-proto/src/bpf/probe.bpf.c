// Gate 0 #7: Minimal eBPF collection prototype
// Hooks sched_switch + sched_wakeup via raw_tp (perf_event_array output)
// Throwaway code — discarded after Gate 0.

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

// Event types
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

// Perf event array (avoid ringbuf to isolate kernel compat as a variable)
struct {
    __uint(type, BPF_MAP_TYPE_PERF_EVENT_ARRAY);
    __uint(key_size, sizeof(int));
    __uint(value_size, sizeof(int));
} events SEC(".maps");

// Per-CPU staging area for perf_event_output
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct event);
} heap SEC(".maps");

static __always_inline struct event *get_event(void) {
    __u32 zero = 0;
    return bpf_map_lookup_elem(&heap, &zero);
}

SEC("raw_tp/sched_switch")
int handle_switch(struct bpf_raw_tracepoint_args *ctx)
{
    // raw_tp args: (bool preempt, struct task_struct *prev, struct task_struct *next)
    struct task_struct *prev = (struct task_struct *)ctx->args[1];
    struct task_struct *next = (struct task_struct *)ctx->args[2];

    struct event *e = get_event();
    if (!e) return 0;

    e->type = EVENT_SWITCH;
    e->cpu = bpf_get_smp_processor_id();
    e->prev_pid  = BPF_CORE_READ(prev, pid);
    e->prev_tgid = BPF_CORE_READ(prev, tgid);
    e->next_pid  = BPF_CORE_READ(next, pid);
    e->next_tgid = BPF_CORE_READ(next, tgid);
    e->timestamp_ns = bpf_ktime_get_ns();
    e->prev_state = BPF_CORE_READ(prev, __state);

    bpf_perf_event_output(ctx, &events, BPF_F_CURRENT_CPU, e, sizeof(*e));
    return 0;
}

SEC("raw_tp/sched_wakeup")
int handle_wakeup(struct bpf_raw_tracepoint_args *ctx)
{
    // raw_tp args: (struct task_struct *p)
    struct task_struct *p = (struct task_struct *)ctx->args[0];

    struct event *e = get_event();
    if (!e) return 0;

    e->type = EVENT_WAKEUP;
    e->cpu = bpf_get_smp_processor_id();
    // prev = waker (current task)
    e->prev_pid  = bpf_get_current_pid_tgid() & 0xFFFFFFFF;
    e->prev_tgid = bpf_get_current_pid_tgid() >> 32;
    // next = woken task
    e->next_pid  = BPF_CORE_READ(p, pid);
    e->next_tgid = BPF_CORE_READ(p, tgid);
    e->timestamp_ns = bpf_ktime_get_ns();
    e->prev_state = 0;

    bpf_perf_event_output(ctx, &events, BPF_F_CURRENT_CPU, e, sizeof(*e));
    return 0;
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
