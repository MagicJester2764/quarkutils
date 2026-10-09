/* How a thread is scheduled: its niceness, and the real-time classes.
 *
 * Nice was a program's: every thread of it as nice as the rest, and
 * setpriority on a thread refused. SCHED_FIFO and SCHED_RR were nobody's —
 * pthread_setschedparam answered ENOSYS — and a program that wanted a thread
 * to run the moment it was woken, an audio thread with a buffer to fill, had
 * it wait behind whatever was computing.
 *
 * Without the right to, SCHED_FIFO is refused (EPERM). A FIFO thread woken
 * through a futex while two ordinary threads compute runs within a
 * millisecond, as the median of a hundred. A FIFO thread that computes for
 * three seconds leaves an ordinary one at least four in a hundred of them.
 * And two threads of one program at nice 0 and 10 have between six and
 * twelve to one of the processor (Linux's weights give 9.3).
 *
 * On one processor, whatever the machine has: every thread and child is kept
 * to the last (sched_setaffinity). With more, the threads would each have
 * one, and what is measured is how they share it. It ran only on a machine
 * of one, until a thread could be kept anywhere.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <quark/manifest.h>
#include <quark/syscall.h>

/* The right to run a thread in a real-time class: given where whoever
   starts this holds it. */
QUARK_MANIFEST(QUARK_CAP_REALTIME, 0UL, 0UL);

#define FUTEX_WAIT 0
#define FUTEX_WAKE 1
#define PRIVATE    128

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed = 1;
    }
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static void nap_ms(long ms) {
    struct timespec t = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&t, NULL);
}

static void compute_for(double seconds) {
    double until = now() + seconds;
    while (now() < until) {
    }
}

/* This thread in `policy` at `priority`: 0, or the error. */
static int become(int policy, int priority) {
    struct sched_param p = {.sched_priority = priority};
    return pthread_setschedparam(pthread_self(), policy, &p);
}

/* In a child, with every right to a real-time class given up first. */
static int without_the_right(void) {
    unsigned long me = __syscall0(SYS_GETPID);
    unsigned long cap[4];
    unsigned long room = __syscall3(SYS_CAP_READ, me, 0, (unsigned long)cap);
    for (unsigned long slot = 0; room != QUARK_ERR && slot < room; slot++) {
        if (__syscall3(SYS_CAP_READ, me, slot, (unsigned long)cap) != QUARK_ERR && cap[0] == QUARK_CAP_REALTIME) {
            __syscall1(SYS_CAP_DELETE, slot);
        }
    }
    return become(SCHED_FIFO, 50) == EPERM ? 0 : 1;
}

static atomic_int stop;

static void *spin(void *arg) {
    (void)arg;
    while (!atomic_load(&stop)) {
    }
    return NULL;
}

/* The woken thread's side: FIFO 50, a hundred waits, each one's lateness. */
static int word;
static atomic_int fifo_ok;
static double stamped, late[100];

static void *woken(void *arg) {
    (void)arg;
    /* Not FIFO, nothing to measure: gone at once, not waiting for wakes
       nobody will send. */
    if (become(SCHED_FIFO, 50) != 0) {
        atomic_store(&fifo_ok, -1);
        return NULL;
    }
    atomic_store(&fifo_ok, 1);
    for (int i = 0; i < 100; i++) {
        while (atomic_load((atomic_int *)&word) == 0) {
            syscall(SYS_futex, &word, FUTEX_WAIT | PRIVATE, 0, NULL, NULL, 0);
        }
        late[i] = now() - stamped;
        atomic_store((atomic_int *)&word, 0);
    }
    return NULL;
}

static int by_value(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return x < y ? -1 : x > y;
}

/* A thread that counts as fast as it can, in its class or as nice as it
   says, until told to stop — or, with `seconds`, for that long by the clock:
   a FIFO thread that waited to be told would wait for a thread it may never
   let run, and a test of whether it does would hang rather than fail. */
struct counter {
    atomic_long n;
    int policy, priority, nice, ok;
    double seconds;
};

static void *count(void *arg) {
    struct counter *c = arg;
    c->ok = 1;
    if (c->policy != SCHED_OTHER) {
        c->ok = become(c->policy, c->priority) == 0;
    } else if (c->nice != 0) {
        c->ok = setpriority(PRIO_PROCESS, (id_t)gettid(), c->nice) == 0;
    }
    double until = c->seconds > 0 ? now() + c->seconds : 0;
    while (!atomic_load(&stop) && (until == 0 || now() < until)) {
        atomic_fetch_add_explicit(&c->n, 1, memory_order_relaxed);
    }
    return NULL;
}

/* Every thread of this test, and every child it forks, on one processor —
   the last, since the first takes the clock and every device's interrupt:
   with more, each thread would have one, and what is measured is how they
   share it. */
static int keep_to_one(void) {
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(sysconf(_SC_NPROCESSORS_ONLN) - 1, &set);
    return sched_setaffinity(0, sizeof set, &set);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("rtsched:\n");
    if (keep_to_one() != 0) {
        printf("  FAIL  its threads can be kept to one processor\n");
        printf("rtsched: FAILED\n");
        return 1;
    }

    pid_t child = fork();
    if (child == 0) {
        _exit(without_the_right());
    }
    int status = 0;
    check("without the right, SCHED_FIFO is refused (EPERM)",
          child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);

    /* Woken while two ordinary threads compute, and while the one that woke
       it goes on computing for three milliseconds before it sleeps. */
    pthread_t a, b, r;
    atomic_store(&stop, 0);
    pthread_create(&a, NULL, spin, NULL);
    pthread_create(&b, NULL, spin, NULL);
    pthread_create(&r, NULL, woken, NULL);
    while (atomic_load(&fifo_ok) == 0) {
        nap_ms(1);
    }
    for (int i = 0; i < 100 && atomic_load(&fifo_ok) == 1; i++) {
        nap_ms(5);
        stamped = now();
        atomic_store((atomic_int *)&word, 1);
        syscall(SYS_futex, &word, FUTEX_WAKE | PRIVATE, 1, NULL, NULL, 0);
        compute_for(0.003);
    }
    pthread_join(r, NULL);
    atomic_store(&stop, 1);
    pthread_join(a, NULL);
    pthread_join(b, NULL);
    qsort(late, 100, sizeof late[0], by_value);
    double median = (late[49] + late[50]) / 2;
    check("a FIFO thread woken through a futex runs within a millisecond (median of 100)",
          atomic_load(&fifo_ok) == 1 && median < 0.001);
    printf("  (median %.0f us, worst %.0f us)\n", median * 1e6, late[99] * 1e6);

    /* A FIFO thread computing for three seconds, an ordinary one beside it.
       The ordinary one counts alone first, for a measure of its pace. */
    /* Its pace is the loop it will count in beside the FIFO thread — one
       that calls the clock each time round is a slower loop — over the time
       it was there to count. */
    struct counter alone = {.policy = SCHED_OTHER};
    atomic_store(&stop, 0);
    double from = now();
    pthread_create(&a, NULL, count, &alone);
    nap_ms(500);
    atomic_store(&stop, 1);
    pthread_join(a, NULL);
    double pace = atomic_load(&alone.n) / (now() - from);

    struct counter fifo = {.policy = SCHED_FIFO, .priority = 50, .seconds = 3.0};
    struct counter other = {.policy = SCHED_OTHER};
    atomic_store(&stop, 0);
    pthread_create(&b, NULL, count, &other);
    double began = now();
    pthread_create(&a, NULL, count, &fifo);
    pthread_join(a, NULL);
    double took = now() - began;
    atomic_store(&stop, 1);
    pthread_join(b, NULL);
    double share = pace > 0 && took > 0 ? atomic_load(&other.n) / (pace * took) : 0;
    check("a FIFO thread computing for three seconds leaves an ordinary one at least 4% of them",
          fifo.ok && share >= 0.04);
    printf("  (it had %.1f%% of %.1f s)\n", share * 100, took);

    /* Nice 0 and nice 10, side by side for two seconds. */
    struct counter zero = {.policy = SCHED_OTHER, .nice = 0};
    struct counter ten = {.policy = SCHED_OTHER, .nice = 10};
    atomic_store(&stop, 0);
    pthread_create(&a, NULL, count, &zero);
    pthread_create(&b, NULL, count, &ten);
    nap_ms(2000);
    atomic_store(&stop, 1);
    pthread_join(a, NULL);
    pthread_join(b, NULL);
    double ratio = atomic_load(&ten.n) > 0 ? (double)atomic_load(&zero.n) / atomic_load(&ten.n) : 0;
    check("two threads at nice 0 and nice 10 have between 6 and 12 to 1 of the processor",
          ten.ok && ratio >= 6 && ratio <= 12);
    printf("  (%.1f to 1)\n", ratio);

    printf("rtsched: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
