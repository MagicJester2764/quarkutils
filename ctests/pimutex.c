/* Mutexes that lend their holder the place of whoever waits for them:
 * PTHREAD_PRIO_INHERIT, Linux's FUTEX_LOCK_PI and FUTEX_UNLOCK_PI.
 *
 * Nothing answered them. musl asks the kernel whether it can lock one when a
 * program says it wants one (pthread_mutexattr_setprotocol), and was told
 * ENOSYS. Without them a thread that holds what a real-time thread waits for
 * runs at its own place, and anything of a priority between the two keeps
 * the processor while the real-time thread waits: priority inversion.
 *
 * A normal thread holds a PI mutex and needs a tenth of a second more to let
 * it go; a FIFO 50 thread waits for it, and a FIFO 10 one computes: the
 * waiter has the mutex within a second. Without the lending the holder runs
 * only in the twentieth of each second the FIFO 10 thread leaves it, and
 * needs two of them. A child process that holds a robust PI mutex in shared
 * memory ends while its parent waits: the parent is answered EOWNERDEAD. Two
 * threads each hold one and lock the other's: one of them is answered
 * EDEADLK. And a child process lent a FIFO 50 waiter's place keeps a normal
 * thread from the processor while it computes; killed, the waiter has the
 * mutex, and a normal thread shares the processor again as normal threads
 * do — the lending went with the child.
 *
 * On one processor, as rtsched: with more, the threads would each have one.
 * It says so and passes. Every lock has a deadline, so that a kernel that
 * gets this wrong fails the test rather than hanging it.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <quark/manifest.h>

/* The right to run a thread in a real-time class. */
QUARK_MANIFEST(QUARK_CAP_REALTIME, 0UL, 0UL);

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

/* A lock that gives up `seconds` from now. */
static int lock_within(pthread_mutex_t *m, int seconds) {
    struct timespec at;
    clock_gettime(CLOCK_REALTIME, &at);
    at.tv_sec += seconds;
    return pthread_mutex_timedlock(m, &at);
}

/* A PI mutex of `type`, robust and shared between processes if `shared`. */
static int pi_init(pthread_mutex_t *m, int type, int shared) {
    pthread_mutexattr_t a;
    pthread_mutexattr_init(&a);
    pthread_mutexattr_settype(&a, type);
    int r = pthread_mutexattr_setprotocol(&a, PTHREAD_PRIO_INHERIT);
    if (r == 0 && shared) {
        r = pthread_mutexattr_setpshared(&a, PTHREAD_PROCESS_SHARED);
    }
    if (r == 0 && shared) {
        r = pthread_mutexattr_setrobust(&a, PTHREAD_MUTEX_ROBUST);
    }
    if (r == 0) {
        r = pthread_mutex_init(m, &a);
    }
    pthread_mutexattr_destroy(&a);
    return r;
}

/* A normal thread counting as fast as it can until told to stop. */
static atomic_int stop;

struct counter {
    atomic_long n;
};

static void *count(void *arg) {
    struct counter *c = arg;
    while (!atomic_load(&stop)) {
        atomic_fetch_add_explicit(&c->n, 1, memory_order_relaxed);
    }
    return NULL;
}

/* 1. Inversion: a holder, a FIFO 50 waiter, a FIFO 10 thread computing. */
static pthread_mutex_t inv;
static atomic_int held, waiting;
static double waited_from, got_at;
static int waiter_said = -1;

static void *holder(void *arg) {
    (void)arg;
    pthread_mutex_lock(&inv);
    atomic_store(&held, 1);
    /* While the waiter and the FIFO 10 thread come, and then what is left
       of what it holds the mutex for: more than a throttle leaves it. */
    nap_ms(50);
    compute_for(0.1);
    pthread_mutex_unlock(&inv);
    return NULL;
}

static void *waiter(void *arg) {
    (void)arg;
    if (become(SCHED_FIFO, 50) != 0) {
        atomic_store(&waiting, -1);
        return NULL;
    }
    waited_from = now();
    atomic_store(&waiting, 1);
    waiter_said = lock_within(&inv, 5);
    got_at = now();
    if (waiter_said == 0) {
        pthread_mutex_unlock(&inv);
    }
    return NULL;
}

static void *middle(void *arg) {
    (void)arg;
    if (become(SCHED_FIFO, 10) == 0) {
        compute_for(3.0);
    }
    return NULL;
}

/* 3. Two threads, each holding one and locking the other's. */
static pthread_mutex_t m1, m2;
static atomic_int a_holds, b_holds;
static int a_said = -1, b_said = -1;

static void *takes_1_then_2(void *arg) {
    (void)arg;
    pthread_mutex_lock(&m1);
    atomic_store(&a_holds, 1);
    while (!atomic_load(&b_holds)) {
        nap_ms(1);
    }
    a_said = lock_within(&m2, 2);
    if (a_said == 0) {
        pthread_mutex_unlock(&m2);
    }
    pthread_mutex_unlock(&m1);
    return NULL;
}

static void *takes_2_then_1(void *arg) {
    (void)arg;
    pthread_mutex_lock(&m2);
    atomic_store(&b_holds, 1);
    while (!atomic_load(&a_holds)) {
        nap_ms(1);
    }
    /* The other is waiting for this one's by now. */
    nap_ms(50);
    b_said = lock_within(&m1, 2);
    if (b_said == 0) {
        pthread_mutex_unlock(&m1);
    }
    pthread_mutex_unlock(&m2);
    return NULL;
}

/* 4. A holder lent a FIFO 50 waiter's place, killed. */
struct shared {
    pthread_mutex_t m;
    atomic_int locked;
};

static struct shared *sh;
static pid_t lent;
static struct counter normal;
static long normal_at_wait, normal_at_kill;
static double wait_began, killed_at, w_got;
static int w_said = -1;

static void *lent_waiter(void *arg) {
    (void)arg;
    if (become(SCHED_FIFO, 50) != 0) {
        return NULL;
    }
    normal_at_wait = atomic_load(&normal.n);
    wait_began = now();
    w_said = lock_within(&sh->m, 5);
    w_got = now();
    if (w_said == EOWNERDEAD) {
        pthread_mutex_consistent(&sh->m);
    }
    if (w_said == 0 || w_said == EOWNERDEAD) {
        pthread_mutex_unlock(&sh->m);
    }
    return NULL;
}

static void *killer(void *arg) {
    (void)arg;
    if (become(SCHED_FIFO, 60) != 0) {
        return NULL;
    }
    nap_ms(300);
    normal_at_kill = atomic_load(&normal.n);
    killed_at = now();
    kill(lent, SIGKILL);
    return NULL;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("pimutex:\n");
    if (sysconf(_SC_NPROCESSORS_ONLN) > 1) {
        printf("pimutex: one processor only, and this machine has %ld: passed\n", sysconf(_SC_NPROCESSORS_ONLN));
        return 0;
    }

    /* 1. */
    int r = pi_init(&inv, PTHREAD_MUTEX_NORMAL, 0);
    check("a priority-inheriting mutex can be made", r == 0);
    if (r != 0) {
        printf("  (it said %d)\n", r);
        printf("pimutex: FAILED\n");
        return 1;
    }
    pthread_t h, w, mid;
    pthread_create(&h, NULL, holder, NULL);
    while (!atomic_load(&held)) {
        nap_ms(1);
    }
    pthread_create(&w, NULL, waiter, NULL);
    while (!atomic_load(&waiting)) {
        nap_ms(1);
    }
    /* The waiter is waiting by now: only then the thread between them. */
    nap_ms(10);
    pthread_create(&mid, NULL, middle, NULL);
    pthread_join(w, NULL);
    pthread_join(h, NULL);
    pthread_join(mid, NULL);
    double took = got_at - waited_from;
    check("a FIFO 50 thread waiting on a normal one's PI mutex has it within a second, a FIFO 10 one computing",
          atomic_load(&waiting) == 1 && waiter_said == 0 && took < 1.0);
    printf("  (it waited %.0f ms, and was told %d)\n", took * 1000, waiter_said);

    /* 2. */
    struct shared *s2 = mmap(NULL, sizeof *s2, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    r = s2 == MAP_FAILED ? -1 : pi_init(&s2->m, PTHREAD_MUTEX_NORMAL, 1);
    int said = -1;
    if (r == 0) {
        atomic_store(&s2->locked, 0);
        pid_t c = fork();
        if (c == 0) {
            pthread_mutex_lock(&s2->m);
            atomic_store(&s2->locked, 1);
            nap_ms(200);
            _exit(0);
        }
        while (c > 0 && !atomic_load(&s2->locked)) {
            nap_ms(1);
        }
        said = lock_within(&s2->m, 5);
        if (said == EOWNERDEAD) {
            pthread_mutex_consistent(&s2->m);
        }
        if (said == 0 || said == EOWNERDEAD) {
            pthread_mutex_unlock(&s2->m);
        }
        waitpid(c, NULL, 0);
    }
    check("a child holding a robust PI mutex ends while its parent waits: EOWNERDEAD", said == EOWNERDEAD);
    printf("  (it was told %d)\n", said);

    /* 3. */
    r = pi_init(&m1, PTHREAD_MUTEX_ERRORCHECK, 0);
    if (r == 0) {
        r = pi_init(&m2, PTHREAD_MUTEX_ERRORCHECK, 0);
    }
    if (r == 0) {
        pthread_t a, b;
        pthread_create(&a, NULL, takes_1_then_2, NULL);
        pthread_create(&b, NULL, takes_2_then_1, NULL);
        pthread_join(a, NULL);
        pthread_join(b, NULL);
    }
    check("two threads each holding one and locking the other's: one is answered EDEADLK",
          (a_said == EDEADLK && b_said == 0) || (b_said == EDEADLK && a_said == 0));
    printf("  (they were told %d and %d)\n", a_said, b_said);

    /* 4. A normal thread counts throughout; its pace first, alone. */
    sh = mmap(NULL, sizeof *sh, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    r = sh == MAP_FAILED ? -1 : pi_init(&sh->m, PTHREAD_MUTEX_NORMAL, 1);
    if (r == 0) {
        atomic_store(&sh->locked, 0);
        atomic_store(&stop, 0);
        pthread_t n, k, lw;
        double from = now();
        pthread_create(&n, NULL, count, &normal);
        nap_ms(200);
        double pace = atomic_load(&normal.n) / (now() - from);
        lent = fork();
        if (lent == 0) {
            pthread_mutex_lock(&sh->m);
            atomic_store(&sh->locked, 1);
            for (;;) {
            }
        }
        while (lent > 0 && !atomic_load(&sh->locked)) {
            nap_ms(1);
        }
        pthread_create(&k, NULL, killer, NULL);
        pthread_create(&lw, NULL, lent_waiter, NULL);
        pthread_join(lw, NULL);
        pthread_join(k, NULL);
        waitpid(lent, NULL, 0);
        double held_for = killed_at - wait_began;
        double share_lent = pace > 0 && held_for > 0 ? (normal_at_kill - normal_at_wait) / (pace * held_for) : 1;
        check("a child lent a FIFO 50 waiter's place keeps a normal thread from the processor (under 10%)",
              share_lent < 0.10);
        printf("  (the normal thread had %.1f%% of %.0f ms)\n", share_lent * 100, held_for * 1000);
        check("killed, the waiter has the mutex within half a second: EOWNERDEAD",
              w_said == EOWNERDEAD && w_got - killed_at < 0.5);
        printf("  (it was told %d, %.0f ms after the kill)\n", w_said, (w_got - killed_at) * 1000);

        /* And a normal thread made now shares the processor with it as a
           normal thread does: nothing kept the child's lent place. */
        struct counter other = {0};
        pthread_t o;
        pthread_create(&o, NULL, count, &other);
        nap_ms(100);
        long n0 = atomic_load(&normal.n), o0 = atomic_load(&other.n);
        nap_ms(500);
        long n1 = atomic_load(&normal.n), o1 = atomic_load(&other.n);
        atomic_store(&stop, 1);
        pthread_join(o, NULL);
        pthread_join(n, NULL);
        double ratio = o1 > o0 ? (double)(n1 - n0) / (o1 - o0) : 0;
        check("after it, two normal threads share the processor (between 1 to 2 and 2 to 1)",
              ratio >= 0.5 && ratio <= 2.0);
        printf("  (%.2f to 1)\n", ratio);
    } else {
        check("a robust PI mutex can be put in shared memory", 0);
    }

    printf("pimutex: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
