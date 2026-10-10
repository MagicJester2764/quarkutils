/* A thousand threads that compute, spread over every processor.
 *
 * A thousand threads of one program each compute for fifty milliseconds of
 * their own time, noting every processor they find themselves on, and are
 * joined. Between them they ran on every processor, and /proc/stat says
 * every processor was busy — in the program or in the kernel — for most of
 * the time they took. Sixteen threads a program could have once, on one
 * processor; then a thousand, on one; this is a thousand on all of them.
 *
 * `thousand one` keeps the program to the first processor before it makes
 * any: on a machine of more than one, the last three checks fail, which is
 * what they are for.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define N 1000
#define MOST 256

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed = 1;
    }
}

static atomic_ullong seen[MOST / 64];
static atomic_int finished;

static long ms_of(clockid_t clock) {
    struct timespec t;
    clock_gettime(clock, &t);
    return t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

static void *computes(void *arg) {
    unsigned long x = (unsigned long)arg;
    long start = ms_of(CLOCK_THREAD_CPUTIME_ID);
    do {
        for (int i = 0; i < 100000; i++) {
            x = x * 6364136223846793005UL + 1442695040888963407UL;
            __asm__ volatile("" : "+r"(x));
        }
        int cpu = sched_getcpu();
        if (cpu >= 0 && cpu < MOST) {
            atomic_fetch_or(&seen[cpu / 64], 1ULL << (cpu % 64));
        }
    } while (ms_of(CLOCK_THREAD_CPUTIME_ID) - start < 50);
    atomic_fetch_add(&finished, 1);
    return (void *)(x | 1);
}

/* Each processor's hundredths busy and idle, from its `cpuN` line of
   /proc/stat; how many lines there were. */
static int times_of(unsigned long long *busy, unsigned long long *idle) {
    FILE *f = fopen("/proc/stat", "r");
    if (!f) {
        return -1;
    }
    char line[512];
    int lines = 0;
    while (fgets(line, sizeof line, f)) {
        unsigned long long user, nice, sys, quiet;
        int n;
        int numbered = strncmp(line, "cpu", 3) == 0 && line[3] >= '0' && line[3] <= '9';
        if (numbered && sscanf(line, "cpu%d %llu %llu %llu %llu", &n, &user, &nice, &sys, &quiet) == 5 && n >= 0 && n < MOST) {
            busy[n] = user + nice + sys;
            idle[n] = quiet;
            lines++;
        }
    }
    fclose(f);
    return lines;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("thousand:\n");
    int n = (int)sysconf(_SC_NPROCESSORS_ONLN);
    if (argc > 1 && strcmp(argv[1], "one") == 0) {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(0, &set);
        sched_setaffinity(0, sizeof set, &set);
        printf("  (kept to the first processor)\n");
    }

    static unsigned long long busy0[MOST], idle0[MOST], busy1[MOST], idle1[MOST];
    int lines = times_of(busy0, idle0);
    long w0 = ms_of(CLOCK_MONOTONIC);

    static pthread_t t[N];
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 16384);
    int made = 0;
    while (made < N && pthread_create(&t[made], &attr, computes, (void *)(long)made) == 0) {
        made++;
    }
    int joined = 0;
    for (int i = 0; i < made; i++) {
        void *got = NULL;
        if (pthread_join(t[i], &got) == 0 && got != NULL) {
            joined++;
        }
    }
    long wall = ms_of(CLOCK_MONOTONIC) - w0;
    times_of(busy1, idle1);

    int ran_on = 0;
    for (int i = 0; i < n && i < MOST; i++) {
        if (atomic_load(&seen[i / 64]) & (1ULL << (i % 64))) {
            ran_on++;
        }
    }
    printf("  (%d made, %d joined, in %ld ms, on %d of %d processors)\n", made, joined, wall, ran_on, n);
    check("a thousand threads are made", made == N);
    check("and every one computes its fifty milliseconds and is joined", joined == made && atomic_load(&finished) == made);
    check("between them they ran on every processor", ran_on == n);

    int quiet = 0;
    printf("  busy:");
    for (int i = 0; i < n && i < MOST; i++) {
        unsigned long long b = busy1[i] - busy0[i], q = idle1[i] - idle0[i];
        int share = b + q == 0 ? 0 : (int)(b * 100 / (b + q));
        printf(" %d%%", share);
        if (share < 50) {
            quiet++;
        }
    }
    printf("\n");
    check("/proc/stat has a line for every processor", lines == n);
    check("and every processor was busy for most of the time they took", lines == n && quiet == 0);

    printf("thousand: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
