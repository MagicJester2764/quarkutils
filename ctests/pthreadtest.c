/* The corners of POSIX threads a toolkit or a runtime leans on.
 *
 * A thread is cancelled where it waits — in a read, a condition wait and a
 * sleep — and its cleanup handlers run; it has a name that it and others
 * can read and that /proc says; the first thread's attributes say where its
 * stack is; a mutex and a condition in shared memory work across a fork; a
 * robust mutex whose owner died says so, whether a thread or a whole
 * program died holding it; one thread's processor time can be read by
 * another, and is its own; a thread is told it may run on every processor;
 * a barrier holds every thread until the last comes; a lock on readers and
 * writers gives up when its time is up. Before: a cancelled thread waited
 * on for ever, no names, a first stack of one page, memory shared after a
 * fork that was not, no robust mutexes at all, and another thread's clock
 * that was the time since the machine started.
 *
 * A watchdog ends the test at a minute: what it is about can hang.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static long ms_since(const struct timespec *t0)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (t.tv_sec - t0->tv_sec) * 1000 + (t.tv_nsec - t0->tv_nsec) / 1000000;
}

/* Cancellation. */
static volatile int cleaned;
static int pipe_ends[2];
static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;

static void cleanup(void *arg)
{
    cleaned += (int)(long)arg;
}

static void *reads(void *arg)
{
    (void)arg;
    char c;
    pthread_cleanup_push(cleanup, (void *)1);
    read(pipe_ends[0], &c, 1);
    pthread_cleanup_pop(0);
    return NULL;
}

static void *waits(void *arg)
{
    (void)arg;
    pthread_mutex_lock(&m);
    pthread_cleanup_push(cleanup, (void *)10);
    for (;;)
        pthread_cond_wait(&cv, &m);
    pthread_cleanup_pop(0);
    return NULL;
}

static void *sleeps(void *arg)
{
    (void)arg;
    pthread_cleanup_push(cleanup, (void *)100);
    sleep(30);
    pthread_cleanup_pop(0);
    return NULL;
}

/* Cancelled at the wait `fn` makes: joined as PTHREAD_CANCELED, soon, its
   cleanup having run. */
static int cancelled(void *(*fn)(void *), int mark)
{
    pthread_t t;
    cleaned = 0;
    if (pthread_create(&t, NULL, fn, NULL) != 0)
        return 0;
    usleep(50000);
    struct timespec t0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    pthread_cancel(t);
    void *ret = NULL;
    pthread_join(t, &ret);
    if (fn == waits)
        pthread_mutex_unlock(&m);
    return ret == PTHREAD_CANCELED && cleaned == mark && ms_since(&t0) < 2000;
}

/* Names. */
static void *named(void *arg)
{
    (void)arg;
    usleep(200000);
    return NULL;
}

/* Robust mutexes. */
static pthread_mutex_t *robust;

static void *dies_holding(void *arg)
{
    (void)arg;
    pthread_mutex_lock(robust);
    return NULL;
}

/* Processor time. */
static volatile int spin_stop;

static void *spins(void *arg)
{
    (void)arg;
    while (!spin_stop)
        ;
    return NULL;
}

static void *idles(void *arg)
{
    (void)arg;
    while (!spin_stop)
        usleep(10000);
    return NULL;
}

static long cpu_ms(clockid_t c)
{
    struct timespec t = {0, 0};
    if (clock_gettime(c, &t) != 0)
        return -1;
    return t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

static void watchdog(int sig)
{
    (void)sig;
    /* What was said so far, which a console that is not a terminal holds
       until the end: the main thread is waiting, not writing. */
    fflush(stdout);
    static const char said[] = "pthreadtest: still going after a minute: FAILED\n";
    write(1, said, sizeof said - 1);
    _exit(1);
}

/* Barriers. */
static pthread_barrier_t barrier;
static volatile int arrived, passed_early;

static void *meets(void *arg)
{
    (void)arg;
    __atomic_add_fetch(&arrived, 1, __ATOMIC_SEQ_CST);
    int r = pthread_barrier_wait(&barrier);
    if (__atomic_load_n(&arrived, __ATOMIC_SEQ_CST) < 4)
        passed_early = 1;
    return (void *)(long)(r == PTHREAD_BARRIER_SERIAL_THREAD);
}

int main(void)
{
    printf("pthreadtest:\n");
    signal(SIGALRM, watchdog);
    alarm(60);

    pipe(pipe_ends);
    check("cancelled in a read, a thread's cleanup runs and it is joined as cancelled", cancelled(reads, 1));
    check("and in a condition wait", cancelled(waits, 10));
    check("and in a sleep", cancelled(sleeps, 100));

    /* Names. */
    pthread_t t;
    pthread_create(&t, NULL, named, NULL);
    check("a thread is named", pthread_setname_np(t, "worker") == 0);
    char name[32] = "";
    check("and its name is read back", pthread_getname_np(t, name, sizeof name) == 0 && strcmp(name, "worker") == 0);
    pid_t tid = 0;
    /* /proc names it by task id, which the thread says itself; here the
       main thread's own is the one asked for. */
    char path[64], comm[32] = "";
    check("a name of sixteen bytes is too long", pthread_setname_np(pthread_self(), "pthreadtest-main") == ERANGE);
    int named_self = pthread_setname_np(pthread_self(), "ptest-main") == 0;
    snprintf(path, sizeof path, "/proc/self/task/%d/comm", (int)gettid());
    int fd = open(path, O_RDONLY);
    ssize_t n = fd >= 0 ? read(fd, comm, sizeof comm - 1) : -1;
    if (fd >= 0)
        close(fd);
    check("/proc/self/task/TID/comm says the thread's name", named_self && n == 11 && strncmp(comm, "ptest-main\n", 11) == 0);
    (void)tid;
    pthread_join(t, NULL);

    /* The first thread's stack. */
    pthread_attr_t a;
    void *stack = NULL;
    size_t size = 0;
    int here = 0;
    check("the first thread's attributes are had",
          pthread_getattr_np(pthread_self(), &a) == 0 && pthread_attr_getstack(&a, &stack, &size) == 0);
    check("and say where its stack is: a local is in it, and it is more than a page",
          size > 4096 && (char *)&here >= (char *)stack && (char *)&here < (char *)stack + size);

    /* Shared across a fork. */
    struct shared {
        pthread_mutex_t m;
        pthread_cond_t c;
        int ready;
        pthread_mutex_t robust;
    } *sh = mmap(NULL, sizeof *sh, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    check("memory to share", sh != MAP_FAILED);
    if (sh != MAP_FAILED) {
        pthread_mutexattr_t ma;
        pthread_mutexattr_init(&ma);
        pthread_mutexattr_setpshared(&ma, PTHREAD_PROCESS_SHARED);
        pthread_mutex_init(&sh->m, &ma);
        pthread_condattr_t ca;
        pthread_condattr_init(&ca);
        pthread_condattr_setpshared(&ca, PTHREAD_PROCESS_SHARED);
        pthread_cond_init(&sh->c, &ca);
        sh->ready = 0;
        pid_t c = fork();
        if (c == 0) {
            usleep(50000);
            pthread_mutex_lock(&sh->m);
            sh->ready = 1;
            pthread_cond_signal(&sh->c);
            pthread_mutex_unlock(&sh->m);
            _exit(0);
        }
        struct timespec until;
        clock_gettime(CLOCK_REALTIME, &until);
        until.tv_sec += 3;
        pthread_mutex_lock(&sh->m);
        int r = 0;
        while (!sh->ready && r == 0)
            r = pthread_cond_timedwait(&sh->c, &sh->m, &until);
        int got = sh->ready;
        pthread_mutex_unlock(&sh->m);
        waitpid(c, NULL, 0);
        check("a process-shared condition is signalled from a forked child", got && r == 0);

        /* A robust mutex whose owner died: a thread, then a program. */
        pthread_mutexattr_t ra;
        pthread_mutexattr_init(&ra);
        pthread_mutexattr_setpshared(&ra, PTHREAD_PROCESS_SHARED);
        int made = pthread_mutexattr_setrobust(&ra, PTHREAD_MUTEX_ROBUST) == 0;
        check("a mutex can be made robust", made);
        if (!made)
            goto robust_done;
        pthread_mutex_init(&sh->robust, &ra);
        robust = &sh->robust;
        pthread_create(&t, NULL, dies_holding, NULL);
        pthread_join(t, NULL);
        int r1 = pthread_mutex_lock(robust);
        check("a robust mutex a thread died holding says EOWNERDEAD", r1 == EOWNERDEAD);
        if (r1 == EOWNERDEAD)
            pthread_mutex_consistent(robust);
        pthread_mutex_unlock(robust);
        c = fork();
        if (c == 0) {
            pthread_mutex_lock(robust);
            _exit(0);
        }
        waitpid(c, NULL, 0);
        struct timespec soon;
        clock_gettime(CLOCK_REALTIME, &soon);
        soon.tv_sec += 2;
        int r2 = pthread_mutex_timedlock(robust, &soon);
        check("and one a whole program died holding", r2 == EOWNERDEAD);
        if (r2 == EOWNERDEAD) {
            pthread_mutex_consistent(robust);
            pthread_mutex_unlock(robust);
        }
    robust_done:;
    }

    /* Other threads' processor time: one that runs, one that waits. */
    spin_stop = 0;
    pthread_t busy, quiet;
    pthread_create(&busy, NULL, spins, NULL);
    pthread_create(&quiet, NULL, idles, NULL);
    clockid_t bc = 0, qc = 0;
    int cr = pthread_getcpuclockid(busy, &bc) | pthread_getcpuclockid(quiet, &qc);
    long b0 = cpu_ms(bc), q0 = cpu_ms(qc);
    usleep(300000);
    long b1 = cpu_ms(bc), q1 = cpu_ms(qc);
    spin_stop = 1;
    pthread_join(busy, NULL);
    pthread_join(quiet, NULL);
    check("another thread's processor time is read", cr == 0 && b0 >= 0 && b1 >= 0 && q0 >= 0 && q1 >= 0);
    check("and is its own: the one running ran, the one waiting hardly did",
          b1 - b0 >= 100 && b1 - b0 <= 400 && q1 - q0 < 50);

    /* Every processor. */
    cpu_set_t set;
    CPU_ZERO(&set);
    int all = sched_getaffinity(0, sizeof set, &set) == 0 ? CPU_COUNT(&set) : 0;
    check("a thread may run on every processor there is", all >= 1 && all == sysconf(_SC_NPROCESSORS_ONLN));
    int cpu = sched_getcpu();
    check("and says which it is on", cpu >= 0 && cpu < all);

    /* A barrier. */
    pthread_barrier_init(&barrier, NULL, 4);
    pthread_t ts[3];
    arrived = 0;
    passed_early = 0;
    for (int i = 0; i < 3; i++)
        pthread_create(&ts[i], NULL, meets, NULL);
    usleep(50000);
    int early = passed_early;
    __atomic_add_fetch(&arrived, 1, __ATOMIC_SEQ_CST);
    int serial = pthread_barrier_wait(&barrier) == PTHREAD_BARRIER_SERIAL_THREAD;
    for (int i = 0; i < 3; i++) {
        void *r;
        pthread_join(ts[i], &r);
        serial += (int)(long)r;
    }
    check("a barrier holds every thread until the last comes, and one is told it was last",
          !early && !passed_early && serial == 1);
    pthread_barrier_destroy(&barrier);

    /* A lock on readers and writers that gives up. */
    pthread_rwlock_t rw = PTHREAD_RWLOCK_INITIALIZER;
    pthread_rwlock_rdlock(&rw);
    struct timespec t0, until;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    clock_gettime(CLOCK_REALTIME, &until);
    until.tv_nsec += 100000000;
    if (until.tv_nsec >= 1000000000) {
        until.tv_sec++;
        until.tv_nsec -= 1000000000;
    }
    int w = pthread_rwlock_timedwrlock(&rw, &until);
    long waited = ms_since(&t0);
    check("a writer waiting on a reader gives up when its time is up", w == ETIMEDOUT && waited >= 90 && waited < 2000);
    check("and another reader is let in", pthread_rwlock_tryrdlock(&rw) == 0);
    pthread_rwlock_unlock(&rw);
    pthread_rwlock_unlock(&rw);

    printf("pthreadtest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
