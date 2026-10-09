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
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
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

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("affinity:\n");
    int n = (int)sysconf(_SC_NPROCESSORS_ONLN);
    last = n - 1;

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
