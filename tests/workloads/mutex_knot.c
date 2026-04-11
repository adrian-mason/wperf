// SPDX-License-Identifier: Apache-2.0
//
// Synthetic 2-thread mutex contention workload for Knot E2E validation.
//
// Two pthreads alternate locking the same mutex with usleep() inside
// the critical section to force voluntary context switches. This
// produces bidirectional wait-for edges (A→B + B→A) in the WFG,
// which the SCC/Knot detector should identify as a Knot.
//
// Usage: ./mutex_knot [duration_seconds]
//   Default duration: 3 seconds

#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static atomic_int stop_flag = 0;

static void *worker(void *arg) {
    long id = (long)arg;
    unsigned long iterations = 0;

    fprintf(stderr, "worker %ld: tid=%d\n", id, (int)gettid());

    while (!atomic_load(&stop_flag)) {
        pthread_mutex_lock(&mtx);
        usleep(500);
        pthread_mutex_unlock(&mtx);
        usleep(100);
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

    pthread_t t1, t2;
    pthread_create(&t1, NULL, worker, (void *)0);
    pthread_create(&t2, NULL, worker, (void *)1);

    sleep(duration);
    atomic_store(&stop_flag, 1);

    pthread_join(t1, NULL);
    pthread_join(t2, NULL);

    fprintf(stderr, "mutex_knot: done\n");
    return 0;
}
