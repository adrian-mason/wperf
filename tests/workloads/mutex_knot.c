// SPDX-License-Identifier: Apache-2.0
//
// Synthetic 2-thread mutex contention workload for Knot E2E validation.
//
// Two pthreads contend on a single mutex, pinned to the same CPU.
// Each thread holds the lock while busy-waiting (~500us), then yields
// CPU so the other thread can acquire the lock. This forces futex-level
// contention that produces sched_wakeup events with correct waker TIDs,
// creating bidirectional wait-for edges (A->B + B->A) in the WFG.
//
// Usage: ./mutex_knot [duration_seconds]
//   Default duration: 3 seconds

#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static atomic_int stop_flag = 0;

static void busy_wait_us(unsigned int us) {
    struct timespec start, now;
    clock_gettime(CLOCK_MONOTONIC, &start);
    unsigned long target_ns = (unsigned long)us * 1000;
    for (;;) {
        clock_gettime(CLOCK_MONOTONIC, &now);
        unsigned long elapsed =
            (unsigned long)(now.tv_sec - start.tv_sec) * 1000000000UL +
            (unsigned long)(now.tv_nsec - start.tv_nsec);
        if (elapsed >= target_ns)
            break;
    }
}

static void *worker(void *arg) {
    long id = (long)arg;
    unsigned long iterations = 0;

    fprintf(stderr, "worker %ld: tid=%d\n", id, (int)gettid());

    while (!atomic_load(&stop_flag)) {
        pthread_mutex_lock(&mtx);
        busy_wait_us(500);
        pthread_mutex_unlock(&mtx);
        sched_yield();
        iterations++;
    }

    fprintf(stderr, "worker %ld: %lu iterations\n", id, iterations);
    return NULL;
}

int main(int argc, char *argv[]) {
    int duration = 3;
    if (argc > 1)
        duration = atoi(argv[1]);

    fprintf(stderr, "mutex_knot: pid=%d, duration=%ds\n", getpid(), duration);

    cpu_set_t cpuset;
    CPU_ZERO(&cpuset);
    CPU_SET(0, &cpuset);

    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setaffinity_np(&attr, sizeof(cpuset), &cpuset);

    pthread_t t1, t2;
    pthread_create(&t1, &attr, worker, (void *)0);
    pthread_create(&t2, &attr, worker, (void *)1);
    pthread_attr_destroy(&attr);

    sleep(duration);
    atomic_store(&stop_flag, 1);

    pthread_join(t1, NULL);
    pthread_join(t2, NULL);

    fprintf(stderr, "mutex_knot: done\n");
    return 0;
}
