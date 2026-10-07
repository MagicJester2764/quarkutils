/* A hundred threads on one futex word, and moving waiters from one word to
 * another.
 *
 * The kernel kept sixty-four futex waiters for the whole machine, and a
 * wait past them was refused at once: a program of a hundred threads
 * waiting for one thing had thirty-six of them spin. FUTEX_REQUEUE was
 * answered by waking everybody it would have moved, and FUTEX_CMP_REQUEUE —
 * which glibc's condition variables and a good many lock libraries use —
 * not at all.
 *
 * A hundred threads wait on f1. FUTEX_CMP_REQUEUE with f1's value wakes one
 * and moves the other ninety-nine to f2, and says a hundred; a wake of f1
 * then wakes nobody, and a wake of f2 the ninety-nine. With a value f1 does
 * not hold it says EAGAIN and moves nobody. And a condition variable's
 * broadcast to a hundred threads, a thousand times, completes.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/* Linux's numbers. */
#define FUTEX_WAIT        0
#define FUTEX_WAKE        1
#define FUTEX_CMP_REQUEUE 4
#define PRIVATE           128

#define THREADS 100
#define ROUNDS  1000

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

static void nap(long ms) {
    struct timespec t = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&t, NULL);
}

static int f1, f2;
static atomic_int ready, returned;
static long answers[THREADS];

/* One wait on f1, and what it said. */
static void *waiter(void *arg) {
    long i = (long)arg;
    atomic_fetch_add(&ready, 1);
    answers[i] = syscall(SYS_futex, &f1, FUTEX_WAIT | PRIVATE, 0, NULL, NULL, 0);
    atomic_fetch_add(&returned, 1);
    return NULL;
}

/* Until `n` have returned, or `ms` have passed. */
static int returned_by(int n, long ms) {
    for (long t = 0; atomic_load(&returned) < n && t < ms; t += 10) {
        nap(10);
    }
    return atomic_load(&returned);
}

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t all_here = PTHREAD_COND_INITIALIZER, go = PTHREAD_COND_INITIALIZER;
static int arrived, round_;

static void *rounder(void *arg) {
    (void)arg;
    for (int r = 0; r < ROUNDS; r++) {
        pthread_mutex_lock(&lock);
        if (++arrived == THREADS) {
            pthread_cond_signal(&all_here);
        }
        int mine = round_;
        while (round_ == mine) {
            pthread_cond_wait(&go, &lock);
        }
        pthread_mutex_unlock(&lock);
    }
    return NULL;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("futexrequeue:\n");

    pthread_t t[THREADS];
    int started = 0;
    for (; started < THREADS; started++) {
        if (pthread_create(&t[started], NULL, waiter, (void *)(long)started) != 0) {
            break;
        }
    }
    /* Long enough for each to be in its wait. */
    for (long ms = 0; atomic_load(&ready) < started && ms < 5000; ms += 10) {
        nap(10);
    }
    nap(300);
    check("a hundred threads wait on one word", started == THREADS && atomic_load(&returned) == 0);

    errno = 0;
    long moved = syscall(SYS_futex, &f1, FUTEX_CMP_REQUEUE | PRIVATE, 1, (void *)(long)(THREADS - 1), &f2, 0);
    check("FUTEX_CMP_REQUEUE wakes one and moves the rest, and says how many", moved == THREADS);
    if (moved != THREADS) {
        printf("  (it said %ld, errno %d)\n", moved, errno);
    }
    check("and one returns", returned_by(1, 2000) == 1);
    check("a wake of the first word then wakes nobody", syscall(SYS_futex, &f1, FUTEX_WAKE | PRIVATE, INT_MAX, NULL, NULL, 0) == 0);
    long woken = syscall(SYS_futex, &f2, FUTEX_WAKE | PRIVATE, INT_MAX, NULL, NULL, 0);
    check("and a wake of the second wakes the ninety-nine", woken == THREADS - 1);
    check("and they all return", returned_by(THREADS, 5000) == THREADS);
    int fine = 1;
    for (int i = 0; i < started; i++) {
        fine &= answers[i] == 0;
    }
    check("each from a wait that said it was woken", fine);
    for (int i = 0; i < started; i++) {
        pthread_join(t[i], NULL);
    }

    f1 = 1;
    errno = 0;
    long stale = syscall(SYS_futex, &f1, FUTEX_CMP_REQUEUE | PRIVATE, 1, (void *)1L, &f2, 0);
    check("with a value the word does not hold it says EAGAIN", stale == -1 && errno == EAGAIN);

    /* A thousand broadcasts, each to a hundred threads. */
    double began = now();
    int rounders = 0;
    for (; rounders < THREADS; rounders++) {
        if (pthread_create(&t[rounders], NULL, rounder, NULL) != 0) {
            break;
        }
    }
    int done = 0;
    if (rounders == THREADS) {
        for (; done < ROUNDS && now() - began < 300; done++) {
            pthread_mutex_lock(&lock);
            while (arrived < THREADS) {
                pthread_cond_wait(&all_here, &lock);
            }
            arrived = 0;
            round_++;
            pthread_cond_broadcast(&go);
            pthread_mutex_unlock(&lock);
        }
        /* Stopped short, the rest wait for a round that is not coming. */
        for (int i = 0; done == ROUNDS && i < rounders; i++) {
            pthread_join(t[i], NULL);
        }
    }
    check("a condition variable's broadcast to a hundred threads, a thousand times, completes", done == ROUNDS);
    printf("  (%d rounds in %.1f s)\n", done, now() - began);

    printf("futexrequeue: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
