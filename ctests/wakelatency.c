/* How soon a better thread that is woken runs, on a machine whose every
 * processor is busy.
 *
 * Each processor runs a computing thread at nice 0. A FIFO 50 thread, kept
 * to the last processor, waits on a futex; the main thread, kept to the
 * first, writes the time and wakes it, two hundred times. From the time
 * written to the time the woken thread reads, the middle of the two hundred
 * is under a millisecond. Woken onto a processor running something worse,
 * the thread waited for that processor's tick — half of ten milliseconds,
 * as the wake fell — where now the processor is interrupted to run it.
 *
 * On a machine of one processor the two share it, and a wake that makes
 * ready something better gives way at the end of the call that woke it.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <linux/futex.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#include <quark/manifest.h>

/* A real-time class is a right, which the C suite's runner gives a program
   that asks. */
QUARK_MANIFEST(QUARK_CAP_REALTIME, 0UL, 0UL);

#define ROUNDS 200

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed = 1;
    }
}

static long now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000000000L + t.tv_nsec;
}

static void keep_to(int cpu) {
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    pthread_setaffinity_np(pthread_self(), sizeof set, &set);
}

static volatile int stop;
static volatile int word;
static volatile int answered;
static volatile long written;
static long took[ROUNDS];
static int last;

static void *computes(void *arg) {
    (void)arg;
    while (!stop) {
    }
    return NULL;
}

static int fifo;
static int fifo_err;

static void *woken(void *arg) {
    (void)arg;
    struct sched_param param = {.sched_priority = 50};
    fifo_err = pthread_setschedparam(pthread_self(), SCHED_FIFO, &param);
    fifo = fifo_err == 0;
    keep_to(last);
    __atomic_store_n(&answered, -1, __ATOMIC_SEQ_CST);
    if (!fifo) {
        return NULL;
    }
    for (int i = 0; i < ROUNDS; i++) {
        while (word == i) {
            syscall(SYS_futex, &word, FUTEX_WAIT, i, NULL, NULL, 0);
        }
        took[i] = now_ns() - written;
        __atomic_store_n(&answered, i + 1, __ATOMIC_SEQ_CST);
    }
    return NULL;
}

static int by_value(const void *a, const void *b) {
    long x = *(const long *)a, y = *(const long *)b;
    return (x > y) - (x < y);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("wakelatency:\n");
    int n = (int)sysconf(_SC_NPROCESSORS_ONLN);
    last = n - 1;

    /* The computing threads first, free to go anywhere; then the woken
       one, which keeps itself to the last processor; and the waker to the
       first. */
    pthread_t busy[16];
    int nbusy = n < 16 ? n : 16;
    for (int i = 0; i < nbusy; i++) {
        pthread_create(&busy[i], NULL, computes, NULL);
    }
    pthread_t t;
    int made = pthread_create(&t, NULL, woken, NULL) == 0;
    keep_to(0);
    while (made && __atomic_load_n(&answered, __ATOMIC_SEQ_CST) == 0) {
        sched_yield();
    }
    __atomic_store_n(&answered, 0, __ATOMIC_SEQ_CST);
    if (made && !fifo) {
        printf("  (pthread_setschedparam: %d)\n", fifo_err);
    }
    check("a thread made FIFO 50", made && fifo);
    if (made && fifo) {
        struct timespec pause = {0, 2000000};
        for (int i = 0; i < ROUNDS; i++) {
            nanosleep(&pause, NULL);
            written = now_ns();
            __atomic_store_n(&word, i + 1, __ATOMIC_SEQ_CST);
            syscall(SYS_futex, &word, FUTEX_WAKE, 1, NULL, NULL, 0);
            long began = now_ns();
            while (__atomic_load_n(&answered, __ATOMIC_SEQ_CST) <= i && now_ns() - began < 1000000000L) {
                sched_yield();
            }
        }
        pthread_join(t, NULL);
        qsort(took, ROUNDS, sizeof took[0], by_value);
        printf("  (from the wake to its running: median %ld us, worst %ld us, %d processors)\n",
               took[ROUNDS / 2] / 1000, took[ROUNDS - 1] / 1000, n);
        check("woken while every processor computes, it runs within a millisecond (median of 200)",
              took[ROUNDS / 2] < 1000000L);
    }
    stop = 1;
    for (int i = 0; i < nbusy; i++) {
        pthread_join(busy[i], NULL);
    }
    printf("wakelatency: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
