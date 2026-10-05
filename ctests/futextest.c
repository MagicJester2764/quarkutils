/* The futex operations programs use besides FUTEX_WAIT and FUTEX_WAKE.
 *
 * Rust's standard library waits with FUTEX_WAIT_BITSET, which takes a
 * deadline where FUTEX_WAIT takes a span, and takes any answer but a
 * timeout for a wake. The C layer answered it as not implemented, so every
 * thread of a Rust program waiting for a lock, a channel or a condition
 * went round its loop as fast as it could: cargo building the kernel on
 * Quark had four processors busy for a quarter of an hour.
 *
 * And musl's condition variables hand each waiter a broadcast woke on to
 * the next with FUTEX_REQUEUE. Refused, the first woke and the rest slept
 * on for good.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/* Linux's numbers. */
#define WAIT_BITSET    9
#define WAKE_BITSET    10
#define PRIVATE        128
#define CLOCK_WALL     256
#define EVERY_BIT      (-1)

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static long futex(volatile int *word, int op, int val, const struct timespec *at, int bits)
{
    return syscall(SYS_futex, word, op, val, at, NULL, bits);
}

static double now(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

/* What `clock` will read `ms` milliseconds from now. */
static struct timespec in(clockid_t clock, long ms)
{
    struct timespec t;
    clock_gettime(clock, &t);
    t.tv_nsec += (ms % 1000) * 1000000L;
    t.tv_sec += ms / 1000 + t.tv_nsec / 1000000000L;
    t.tv_nsec %= 1000000000L;
    return t;
}

/* A wait that ends at a deadline on `clock`: did it time out, and not before? */
static int ends_at(clockid_t clock, int flag)
{
    volatile int word = 0;
    struct timespec at = in(clock, 200);
    double began = now();
    long r = futex(&word, WAIT_BITSET | PRIVATE | flag, 0, &at, EVERY_BIT);
    int e = errno;
    double took = now() - began;
    return r == -1 && e == ETIMEDOUT && took >= 0.195 && took < 2.0;
}

static volatile int word;
static volatile int waiting;
static long woken_by;

static void *waiter(void *unused)
{
    (void)unused;
    struct timespec at = in(CLOCK_MONOTONIC, 3000);
    waiting = 1;
    woken_by = futex(&word, WAIT_BITSET | PRIVATE, 0, &at, EVERY_BIT);
    return NULL;
}

static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t c = PTHREAD_COND_INITIALIZER;
static int asleep, go, awake;

static void *sleeper(void *unused)
{
    (void)unused;
    pthread_mutex_lock(&m);
    asleep++;
    while (!go)
        pthread_cond_wait(&c, &m);
    awake++;
    pthread_mutex_unlock(&m);
    return NULL;
}

/* Read `*n` under the condition's mutex until it is `want`, for up to `ms`. */
static int reaches(int *n, int want, int ms)
{
    for (int i = 0; i < ms; i++) {
        pthread_mutex_lock(&m);
        int got = *n;
        pthread_mutex_unlock(&m);
        if (got == want)
            return 1;
        usleep(1000);
    }
    return 0;
}

int main(void)
{
    check("FUTEX_WAIT_BITSET times out at a deadline on CLOCK_MONOTONIC, and not before",
          ends_at(CLOCK_MONOTONIC, 0));
    check("and at one on the date's clock (FUTEX_CLOCK_REALTIME)",
          ends_at(CLOCK_REALTIME, CLOCK_WALL));

    volatile int w = 0;
    struct timespec gone = { 0, 0 };
    double began = now();
    long r = futex(&w, WAIT_BITSET | PRIVATE, 0, &gone, EVERY_BIT);
    check("a deadline already past is a timeout at once",
          r == -1 && errno == ETIMEDOUT && now() - began < 0.5);

    w = 1;
    r = futex(&w, WAIT_BITSET | PRIVATE, 0, NULL, EVERY_BIT);
    check("a word that is no longer what was expected is EAGAIN", r == -1 && errno == EAGAIN);
    r = futex(&w, WAIT_BITSET | PRIVATE, 1, NULL, 0);
    check("a set of no bits is refused (EINVAL)", r == -1 && errno == EINVAL);

    pthread_t t;
    pthread_create(&t, NULL, waiter, NULL);
    while (!waiting)
        usleep(1000);
    usleep(100000);
    word = 1;
    long woke = futex(&word, WAKE_BITSET | PRIVATE, 1, NULL, EVERY_BIT);
    pthread_join(t, NULL);
    check("FUTEX_WAKE_BITSET wakes a thread waiting with FUTEX_WAIT_BITSET",
          woke == 1 && woken_by == 0);

    pthread_t s[3];
    for (int i = 0; i < 3; i++)
        pthread_create(&s[i], NULL, sleeper, NULL);
    int all_asleep = reaches(&asleep, 3, 2000);
    pthread_mutex_lock(&m);
    go = 1;
    pthread_cond_broadcast(&c);
    pthread_mutex_unlock(&m);
    int all_awake = all_asleep && reaches(&awake, 3, 3000);
    check("pthread_cond_broadcast wakes all three of a condition's waiters", all_awake);
    if (all_awake)
        for (int i = 0; i < 3; i++)
            pthread_join(s[i], NULL);

    printf("futextest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
