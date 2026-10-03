/* What a C program is told it has used, and what it may say about how it
 * runs. getrusage, times and the processor-time clocks count what was
 * computed; a child's use is its parent's once collected, with what its own
 * children used, and wait4 says what that child's was. nice and setpriority
 * make a program nicer, and a child it forks is as nice. RLIMIT_CPU tells a
 * program with SIGXCPU once a second past the soft limit, ends one that has
 * said nothing about it, and kills one at the hard limit; a child inherits
 * the limit.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/times.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static long ms_of(clockid_t clock) {
    struct timespec ts;
    if (clock_gettime(clock, &ts)) {
        return -1;
    }
    return ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static long ms_tv(struct timeval tv) {
    return tv.tv_sec * 1000 + tv.tv_usec / 1000;
}

/* In microseconds and then in milliseconds: each part rounded down on its
   own would make 200.4 ms of 150.7 and 49.7 into 199. */
static long ms_used(const struct rusage *r) {
    return (r->ru_utime.tv_sec + r->ru_stime.tv_sec) * 1000 +
           (r->ru_utime.tv_usec + r->ru_stime.tv_usec) / 1000;
}

static volatile unsigned long sink;

static void some_work(void) {
    unsigned long x = sink;
    for (int i = 0; i < 100000; i++) {
        x = x * 6364136223846793005UL + 1;
    }
    sink = x;
}

/* Compute until `clock` says `ms` more have gone by: the processor's time
   for this process, or the time on the wall. Or until ten times as long has
   gone by on the wall, with something wrong. */
static void compute_ms(clockid_t clock, long ms) {
    long until = ms_of(clock) + ms;
    long give_up = ms_of(CLOCK_MONOTONIC) + 10 * ms + 2000;
    while (ms_of(clock) < until && ms_of(CLOCK_MONOTONIC) < give_up) {
        some_work();
    }
}

static volatile sig_atomic_t xcpu;

static void on_xcpu(int sig) {
    (void)sig;
    xcpu++;
}

/* Run `body` in a child and say how it ended: its exit status, or the
   negated signal that ended it. */
static int in_child(int (*body)(void)) {
    pid_t pid = fork();
    if (pid == 0) {
        _exit(body());
    }
    int st;
    if (pid < 0 || waitpid(pid, &st, 0) != pid) {
        return 1000;
    }
    return WIFEXITED(st) ? WEXITSTATUS(st) : WIFSIGNALED(st) ? -WTERMSIG(st) : 1001;
}

static int computes(void) {
    compute_ms(CLOCK_PROCESS_CPUTIME_ID, 200);
    return 0;
}

static int computes_by_a_child(void) {
    return in_child(computes);
}

static int told_twice(void) {
    struct rlimit rl = {1, 3};
    if (setrlimit(RLIMIT_CPU, &rl)) {
        return 100;
    }
    signal(SIGXCPU, on_xcpu);
    long give_up = ms_of(CLOCK_MONOTONIC) + 30000;
    while (xcpu < 2 && ms_of(CLOCK_PROCESS_CPUTIME_ID) < 2900 && ms_of(CLOCK_MONOTONIC) < give_up) {
        some_work();
    }
    long at = ms_of(CLOCK_PROCESS_CPUTIME_ID);
    return xcpu == 2 && at >= 2000 ? 0 : 10 + xcpu;
}

static int computes_long(void) {
    compute_ms(CLOCK_PROCESS_CPUTIME_ID, 5000);
    return 0;
}

static int limits_a_child(void) {
    struct rlimit rl = {1, RLIM_INFINITY};
    if (setrlimit(RLIMIT_CPU, &rl)) {
        return 100;
    }
    return in_child(computes_long) == -SIGXCPU ? 0 : 1;
}

static int hard_limit(void) {
    struct rlimit rl = {1, 1};
    if (setrlimit(RLIMIT_CPU, &rl)) {
        return 100;
    }
    return computes_long();
}

static int says_its_limit(void) {
    struct rlimit rl = {5, 10}, got;
    int set = setrlimit(RLIMIT_CPU, &rl);
    int read = getrlimit(RLIMIT_CPU, &got);
    struct rlimit upside_down = {8, 6};
    errno = 0;
    int refused = setrlimit(RLIMIT_CPU, &upside_down) == -1 && errno == EINVAL;
    return set == 0 && read == 0 && got.rlim_cur == 5 && got.rlim_max == 10 && refused ? 0 : 1;
}

static int how_nice(void) {
    errno = 0;
    int n = getpriority(PRIO_PROCESS, 0);
    return errno ? 100 : n + 20;
}

int main(void) {
    printf("usagetest: what a program uses, and how it may run\n");

    struct rusage r0, r1;
    struct tms t0, t1;
    getrusage(RUSAGE_SELF, &r0);
    times(&t0);
    long p0 = ms_of(CLOCK_PROCESS_CPUTIME_ID), th0 = ms_of(CLOCK_THREAD_CPUTIME_ID);
    long w0 = ms_of(CLOCK_MONOTONIC);
    compute_ms(CLOCK_MONOTONIC, 300);
    long wall = ms_of(CLOCK_MONOTONIC) - w0;
    getrusage(RUSAGE_SELF, &r1);
    times(&t1);
    long p1 = ms_of(CLOCK_PROCESS_CPUTIME_ID), th1 = ms_of(CLOCK_THREAD_CPUTIME_ID);
    long user = ms_tv(r1.ru_utime) - ms_tv(r0.ru_utime);
    long used = ms_used(&r1) - ms_used(&r0);
    printf("    computing for %ld ms: %ld ms used, %ld of it in the program\n", wall, used, user);
    check("getrusage counts what was computed, and it was in the program", user >= 150 && used <= wall + 5);
    check("the process's clock counts it", p1 - p0 >= 150 && p1 - p0 <= wall + 5);
    check("and the thread's", th1 - th0 >= 150 && th1 <= p1 + 1);
    check("times counts it in ticks", t1.tms_utime - t0.tms_utime >= 15);

    struct rusage c0, c1, ru;
    getrusage(RUSAGE_CHILDREN, &c0);
    pid_t pid = fork();
    if (pid == 0) {
        _exit(computes());
    }
    int st;
    pid_t got = wait4(pid, &st, 0, &ru);
    getrusage(RUSAGE_CHILDREN, &c1);
    long child = ms_used(&ru), children = ms_used(&c1) - ms_used(&c0);
    if (got != pid || child < 200 || child >= 400 || children < 200 || children >= 400) {
        printf("    wait4 for %d: %d (errno %d), status %#x; it used %ld ms, the children %ld more\n",
               (int)pid, (int)got, errno, st, child, children);
    }
    check("wait4 says what the child it collected used", got == pid && child >= 200 && child < 400);
    check("and it is what the parent's children used", children >= 200 && children < 400);
    getrusage(RUSAGE_CHILDREN, &c0);
    int ended = in_child(computes_by_a_child);
    getrusage(RUSAGE_CHILDREN, &c1);
    children = ms_used(&c1) - ms_used(&c0);
    check("with what that child's own children used", ended == 0 && children >= 200 && children < 400);

    errno = 0;
    int n0 = getpriority(PRIO_PROCESS, 0);
    check("a program is as nice as it was made", errno == 0 && n0 >= -20 && n0 <= 19);
    int n1 = nice(3);
    check("nice makes it nicer", n1 == n0 + 3 && getpriority(PRIO_PROCESS, 0) == n0 + 3);
    check("and a child it forks is as nice", in_child(how_nice) == n0 + 23);
    errno = 0;
    int r = setpriority(PRIO_PROCESS, 0, n0);
    check("to be less nice again takes the right to",
          (r == -1 && errno == EACCES) || (r == 0 && getpriority(PRIO_PROCESS, 0) == n0));
    errno = 0;
    check("a process that is not there is not found", getpriority(PRIO_PROCESS, 999999) == -1 && errno == ESRCH);

    check("getrlimit says what setrlimit set, and a soft limit above the hard one is refused",
          in_child(says_its_limit) == 0);
    check("past its soft limit a program is told with SIGXCPU, once a second", in_child(told_twice) == 0);
    check("one that has said nothing is ended by it, and a child inherits the limit",
          in_child(limits_a_child) == 0);
    check("at its hard limit a program is killed", in_child(hard_limit) == -SIGKILL);

    printf("usagetest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
