/* A program's timers: POSIX's timer_create, timer_settime, timer_gettime,
 * timer_getoverrun and timer_delete.
 *
 * A timer raises a signal when it is due, carrying its value, and says it
 * is a timer's; one that fires while its last signal still waits raises no
 * other, and counts the overruns; it can be set for a time on its clock as
 * well as for a while from now; SIGEV_THREAD runs a function in a thread of
 * musl's, again and again; and a forked child has none of them. The layer
 * answered all five with ENOSYS, and programs that asked fell back to the
 * one alarm a program has.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define MS 1000000L

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static volatile int fired, fired_value, fired_code, fired_id;

static void on_timer(int sig, siginfo_t *si, void *uc)
{
    (void)sig;
    (void)uc;
    fired++;
    fired_value = si->si_value.sival_int;
    fired_code = si->si_code;
    fired_id = si->si_timerid;
}

static volatile int notified, notified_value;

static void notify(union sigval v)
{
    notified_value = v.sival_int;
    __atomic_add_fetch(&notified, 1, __ATOMIC_SEQ_CST);
}

static long ms_between(const struct timespec *a, const struct timespec *b)
{
    return (b->tv_sec - a->tv_sec) * 1000 + (b->tv_nsec - a->tv_nsec) / MS;
}

/* Until `*what` is at least `n`, or a second has gone. */
static void wait_for(volatile int *what, int n)
{
    for (int i = 0; i < 1000 && *what < n; i++)
        usleep(1000);
}

static void handle(int sig)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_timer;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
}

int main(void)
{
    printf("ptimertest:\n");
    int sig = SIGRTMIN;
    handle(sig);

    /* A signal for the program, run by a handler. */
    struct sigevent ev;
    memset(&ev, 0, sizeof ev);
    ev.sigev_notify = SIGEV_SIGNAL;
    ev.sigev_signo = sig;
    ev.sigev_value.sival_int = 42;
    timer_t t;
    check("a timer is made", timer_create(CLOCK_MONOTONIC, &ev, &t) == 0);
    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 20 * MS;
    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    check("and set", timer_settime(t, 0, &its, NULL) == 0);
    struct itimerspec now;
    timer_gettime(t, &now);
    check("it says how long is left", now.it_value.tv_sec == 0 && now.it_value.tv_nsec > 0 && now.it_value.tv_nsec <= 20 * MS);
    wait_for(&fired, 1);
    clock_gettime(CLOCK_MONOTONIC, &end);
    check("its signal runs the handler once it is due", fired == 1 && ms_between(&start, &end) >= 20);
    check("told it is the timer's, with its number and its value",
          fired_code == SI_TIMER && fired_value == 42 && fired_id == (int)(intptr_t)t);
    timer_gettime(t, &now);
    check("and then it is disarmed", now.it_value.tv_sec == 0 && now.it_value.tv_nsec == 0);

    /* Every millisecond, its signal held back: one signal, and overruns. */
    sigset_t set, old;
    sigemptyset(&set);
    sigaddset(&set, sig);
    sigprocmask(SIG_BLOCK, &set, &old);
    its.it_value.tv_nsec = MS;
    its.it_interval.tv_nsec = MS;
    timer_settime(t, 0, &its, NULL);
    usleep(40000);
    struct itimerspec was;
    memset(&its, 0, sizeof its);
    timer_settime(t, 0, &its, &was);
    check("setting it says how it was", was.it_interval.tv_sec == 0 && was.it_interval.tv_nsec == MS);
    siginfo_t si;
    struct timespec none = {0, 0};
    int w = sigtimedwait(&set, &si, &none);
    int overrun = si.si_overrun;
    check("one that fires while its signal waits raises no other, and that one counts the overruns",
          w == sig && overrun >= 10 && sigtimedwait(&set, &si, &none) == -1);
    check("timer_getoverrun says the same", timer_getoverrun(t) == overrun);
    sigprocmask(SIG_SETMASK, &old, NULL);

    /* For a time on the date's clock. */
    timer_t r;
    ev.sigev_value.sival_int = 7;
    timer_create(CLOCK_REALTIME, &ev, &r);
    struct timespec when;
    clock_gettime(CLOCK_REALTIME, &when);
    when.tv_nsec += 30 * MS;
    if (when.tv_nsec >= 1000 * MS) {
        when.tv_sec++;
        when.tv_nsec -= 1000 * MS;
    }
    memset(&its, 0, sizeof its);
    its.it_value = when;
    fired = 0;
    check("a timer is set for a date", timer_settime(r, TIMER_ABSTIME, &its, NULL) == 0);
    wait_for(&fired, 1);
    clock_gettime(CLOCK_REALTIME, &end);
    check("and fires then", fired == 1 && fired_value == 7 &&
                                (end.tv_sec > when.tv_sec || (end.tv_sec == when.tv_sec && end.tv_nsec >= when.tv_nsec)));

    /* A function, run in a thread of the C library's. */
    struct sigevent te;
    memset(&te, 0, sizeof te);
    te.sigev_notify = SIGEV_THREAD;
    te.sigev_notify_function = notify;
    te.sigev_value.sival_int = 9;
    timer_t th;
    check("a timer that runs a function is made", timer_create(CLOCK_MONOTONIC, &te, &th) == 0);
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 10 * MS;
    its.it_interval.tv_nsec = 10 * MS;
    timer_settime(th, 0, &its, NULL);
    wait_for(&notified, 3);
    check("and runs it, again and again, with its value", notified >= 3 && notified_value == 9);
    check("and is ended", timer_delete(th) == 0);

    /* With nothing said: SIGALRM, carrying its own number. */
    handle(SIGALRM);
    timer_t a;
    check("a timer made with nothing said", timer_create(CLOCK_MONOTONIC, NULL, &a) == 0);
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 10 * MS;
    fired = 0;
    timer_settime(a, 0, &its, NULL);
    wait_for(&fired, 1);
    check("raises SIGALRM carrying its own number", fired == 1 && fired_code == SI_TIMER && fired_value == (int)(intptr_t)a);

    /* A forked child has none of them; ended, one is no more. */
    pid_t c = fork();
    if (c == 0)
        _exit(timer_gettime(r, &now) == -1 && errno == EINVAL ? 0 : 1);
    int status = -1;
    waitpid(c, &status, 0);
    check("a forked child has none of its parent's timers", WIFEXITED(status) && WEXITSTATUS(status) == 0);
    check("ended, a timer is no more", timer_delete(t) == 0 && timer_gettime(t, &now) == -1 && errno == EINVAL);
    timer_t x;
    check("a timer on a processor's time is refused",
          timer_create(CLOCK_PROCESS_CPUTIME_ID, &ev, &x) == -1 && errno == EINVAL);
    timer_delete(r);
    timer_delete(a);

    printf("ptimertest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
