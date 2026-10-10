/* Which processors a thread may run on (sched_setaffinity,
 * pthread_setaffinity_np).
 *
 * A thread kept to the last processor asks sched_getcpu ten thousand times
 * while three others compute, and is on its own processor every time;
 * sched_getaffinity says the set that was given; a set with no online
 * processor in it is EINVAL. On a machine of one processor the thread is
 * kept to that one, and the rest holds the same. It was ENOSYS: nothing
 * could keep a thread anywhere, and a test of how threads share a
 * processor could only be run on a machine of one.
 *
 * `affinity loaded` keeps the thread there while twice as many threads as
 * there are processors compute, kept nowhere, and has it ask for a whole
 * second: ten thousand asks are over within a turn, and are never
 * preempted. It is put aside and run again many times, and is on its own
 * processor every time.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed = 1;
    }
}

static volatile int stop;
static int last;
static int kept;
static int away = -1;

static void *computes(void *arg) {
    (void)arg;
    while (!stop) {
    }
    return NULL;
}

static void *kept_to_last(void *arg) {
    (void)arg;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(last, &set);
    kept = pthread_setaffinity_np(pthread_self(), sizeof set, &set) == 0;
    int elsewhere = 0;
    for (int i = 0; i < 10000; i++) {
        if (sched_getcpu() != last) {
            elsewhere++;
        }
    }
    away = elsewhere;
    return NULL;
}

static long ms_now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

static long asks;
static long again;

/* Kept to the last processor, it asks for a second; a gap of two
   milliseconds between two looks at the clock is a turn it did not have. */
static void *kept_while_loaded(void *arg) {
    (void)arg;
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(last, &set);
    kept = pthread_setaffinity_np(pthread_self(), sizeof set, &set) == 0;
    int elsewhere = 0;
    long start = ms_now(), seen = start, now;
    while ((now = ms_now()) - start < 1000) {
        if (now - seen >= 2) {
            again++;
        }
        seen = now;
        for (int i = 0; i < 100; i++) {
            if (sched_getcpu() != last) {
                elsewhere++;
            }
            asks++;
        }
    }
    away = elsewhere;
    return NULL;
}

static int loaded(int n) {
    int many = 2 * n;
    pthread_t *busy = calloc((size_t)many, sizeof *busy);
    int made = 0;
    while (busy && made < many && pthread_create(&busy[made], NULL, computes, NULL) == 0) {
        made++;
    }
    struct timespec spread = {0, 100000000};
    nanosleep(&spread, NULL);
    pthread_t t;
    pthread_create(&t, NULL, kept_while_loaded, NULL);
    pthread_join(t, NULL);
    stop = 1;
    for (int i = 0; i < made; i++) {
        pthread_join(busy[i], NULL);
    }
    free(busy);
    check("a thread may be kept to the last processor", kept);
    printf("  (%ld asks in a second, %d elsewhere; run again %ld times while %d threads computed)\n", asks, away, again, made);
    check("twice as many threads as processors compute", made == many);
    /* Three turns a processor shares are about a tenth of a second each:
       ten in the second is what there is room for, and three is plenty to
       say it was put aside and run again. */
    check("and on that loaded machine it is put aside and run again", again >= 3);
    check("and is on its own processor every time it asks", kept && away == 0);
    printf("affinity: %s\n", failed ? "FAILED" : "passed");
    return failed;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("affinity:\n");
    int n = (int)sysconf(_SC_NPROCESSORS_ONLN);
    last = n - 1;
    if (argc > 1 && strcmp(argv[1], "loaded") == 0) {
        return loaded(n);
    }

    pthread_t others[3], t;
    for (int i = 0; i < 3; i++) {
        pthread_create(&others[i], NULL, computes, NULL);
    }
    pthread_create(&t, NULL, kept_to_last, NULL);
    pthread_join(t, NULL);
    stop = 1;
    for (int i = 0; i < 3; i++) {
        pthread_join(others[i], NULL);
    }
    check("a thread may be kept to the last processor", kept);
    printf("  (%d of 10000 asked elsewhere)\n", away);
    check("and is on it every time it asks, while three others compute", kept && away == 0);

    cpu_set_t set, got;
    CPU_ZERO(&set);
    CPU_SET(0, &set);
    int given = sched_setaffinity(0, sizeof set, &set);
    CPU_ZERO(&got);
    int said = sched_getaffinity(0, sizeof got, &got);
    check("sched_getaffinity says the set sched_setaffinity gave", given == 0 && said == 0 && CPU_EQUAL(&set, &got));

    CPU_ZERO(&set);
    if (n < CPU_SETSIZE) {
        CPU_SET(n, &set);
    }
    errno = 0;
    int none = sched_setaffinity(0, sizeof set, &set);
    check("a set with no online processor in it is EINVAL", none == -1 && errno == EINVAL);

    CPU_ZERO(&set);
    for (int i = 0; i < n; i++) {
        CPU_SET(i, &set);
    }
    sched_setaffinity(0, sizeof set, &set);
    printf("affinity: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
