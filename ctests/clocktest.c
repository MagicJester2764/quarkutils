/* The clock, as a C program has it.
 *
 * For a long time every time here was a multiple of ten milliseconds: the
 * clock was the count of a hundred interrupts a second, a sleep ended on
 * one of them, and a program that drew sixty frames a second asked to be
 * woken in sixteen milliseconds and was woken in twenty. The kernel keeps
 * time in nanoseconds now, where the machine has a counter to keep it by,
 * and ends a wait when it is due.
 *
 * What is asked here is what every C program assumes without asking: that
 * the clock is as fine as it says, that a wait is never short and — on a
 * machine that can — not long either, by every way there is to wait, and
 * that the date can be set by a program that may.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

#include <quark/manifest.h>

/* The right to set the clock, where whoever starts this holds it. */
QUARK_MANIFEST(QUARK_CAP_SET_CLOCK);

#define MS 1000000L

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static long long ns_on(clockid_t clock) {
    struct timespec t;
    clock_gettime(clock, &t);
    return (long long)t.tv_sec * 1000000000LL + t.tv_nsec;
}

static long long now(void) {
    return ns_on(CLOCK_MONOTONIC);
}

static struct timespec span(long long ns) {
    struct timespec t = { ns / 1000000000LL, ns % 1000000000LL };
    return t;
}

/* How a wait went, over several tries: the shortest it took, and how many
   of them were over in less than `soon`. The shortest is held to never
   being short. How many were soon is held to being most of them — not all,
   because a machine with other work is sometimes late, and not one,
   because a wait that ends on the next tick ends soon once in a while by
   where in a tick it began. */
struct went {
    long long least;
    int soon;
};

static struct went timed(int tries, long long soon, void (*wait)(void)) {
    struct went w = { 1LL << 62, 0 };
    for (int i = 0; i < tries; i++) {
        long long from = now();
        wait();
        long long took = now() - from;
        if (took < w.least) {
            w.least = took;
        }
        if (took < soon) {
            w.soon++;
        }
    }
    return w;
}

static void sleep_1ms(void) {
    struct timespec t = span(1 * MS);
    nanosleep(&t, NULL);
}

static void sleep_until_3ms(void) {
    struct timespec t = span(now() + 3 * MS);
    clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &t, NULL);
}

static void usleep_2ms(void) {
    usleep(2000);
}

static void poll_3ms(void) {
    poll(NULL, 0, 3);
}

static void ppoll_1500us(void) {
    struct timespec t = span(1500000);
    ppoll(NULL, 0, &t, NULL);
}

static void select_2ms(void) {
    struct timeval t = { 0, 2000 };
    select(0, NULL, NULL, NULL, &t);
}

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t never = PTHREAD_COND_INITIALIZER;
static int cond_timed_out = 1;

static void cond_3ms(void) {
    struct timespec t = span(ns_on(CLOCK_REALTIME) + 3 * MS);
    pthread_mutex_lock(&lock);
    if (pthread_cond_timedwait(&never, &lock, &t) != ETIMEDOUT) {
        cond_timed_out = 0;
    }
    pthread_mutex_unlock(&lock);
}

static volatile sig_atomic_t alarms;

static void on_alarm(int sig) {
    (void)sig;
    alarms++;
}

static void alarm_5ms(void) {
    struct itimerval it = { { 0, 0 }, { 0, 5000 } };
    sigset_t none;
    sigemptyset(&none);
    int before = alarms;
    setitimer(ITIMER_REAL, &it, NULL);
    while (alarms == before) {
        sigsuspend(&none);
    }
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("the clock:\n");

    long long first = now();
    check("the clock goes forward", first > 0 && now() >= first);

    /* How finely: the smallest step between two readings, over thirty
       milliseconds of reading it. */
    long long step = 1LL << 62, last = now(), until = last + 30 * MS;
    while (last < until) {
        long long t = now();
        if (t != last && t - last < step) {
            step = t - last;
        }
        last = t;
    }
    int fine = step < 10 * MS;
    if (fine) {
        printf("        it moves in steps of %lld ns or less\n", step);
    } else {
        printf("        this machine's clock is the tick: what a finer one does is not checked\n");
    }
    struct timespec res = { 9, 9 };
    check("it says what it is kept in", clock_getres(CLOCK_MONOTONIC, &res) == 0 && res.tv_sec == 0
                                            && res.tv_nsec == 1);

    struct timeval tv;
    gettimeofday(&tv, NULL);
    long long date = ns_on(CLOCK_REALTIME);
    long long by_day = (long long)tv.tv_sec * 1000000000LL + tv.tv_usec * 1000LL;
    check("the date is one date, whichever call is asked",
          date - by_day < 1000 * MS && by_day - date < 1000 * MS && time(NULL) >= tv.tv_sec
              && time(NULL) <= tv.tv_sec + 1);

    /* Every way there is to wait: never short, and where the clock is fine,
       most of the time not long. */
    struct went w = timed(20, 5 * MS, sleep_1ms);
    check("a sleep of a millisecond is a millisecond at least", w.least >= 1 * MS);
    if (fine) {
        check("and not a tick: it ends when the millisecond does", w.soon >= 15);
    }
    w = timed(10, 8 * MS, sleep_until_3ms);
    check("a sleep until a time ends at that time", w.least >= 3 * MS - 100000 && (!fine || w.soon >= 7));
    w = timed(5, 7 * MS, usleep_2ms);
    check("usleep", w.least >= 2 * MS && (!fine || w.soon >= 3));
    w = timed(10, 8 * MS, poll_3ms);
    check("a poll with a time to give up at", w.least >= 3 * MS && (!fine || w.soon >= 7));
    w = timed(10, 6 * MS, ppoll_1500us);
    check("one whose time is not whole milliseconds", w.least >= 1500000 && (!fine || w.soon >= 7));
    w = timed(10, 7 * MS, select_2ms);
    check("select", w.least >= 2 * MS && (!fine || w.soon >= 7));
    w = timed(10, 8 * MS, cond_3ms);
    check("a condition waited for until a time", cond_timed_out && w.least >= 3 * MS - 100000
                                                     && (!fine || w.soon >= 7));

    /* An interval timer and its signal. */
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_alarm;
    sigaction(SIGALRM, &sa, NULL);
    w = timed(5, 15 * MS, alarm_5ms);
    check("an alarm of five milliseconds comes in five", alarms == 5 && w.least >= 5 * MS
                                                             && (!fine || w.soon >= 3));
    struct itimerval far = { { 0, 0 }, { 2, 0 } }, left, off = { { 0, 0 }, { 0, 0 } };
    setitimer(ITIMER_REAL, &far, NULL);
    getitimer(ITIMER_REAL, &left);
    long long left_us = left.it_value.tv_sec * 1000000LL + left.it_value.tv_usec;
    check("what is left of one is said in microseconds", left_us > 1900000 && left_us <= 2000000);
    check("and in seconds, rounded up", alarm(0) == 2);
    setitimer(ITIMER_REAL, &off, NULL);

    /* A timer that is a descriptor. */
    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    check("a timer descriptor", fd >= 0);
    if (fd >= 0) {
        unsigned long long count = 0;
        struct itimerspec every = { { 0, 1 * MS }, { 0, 1 * MS } };
        long long before_set = now();
        int set = timerfd_settime(fd, 0, &every, NULL) == 0;
        long long after_set = now();
        usleep(40000);
        long long before_read = now();
        int got = read(fd, &count, sizeof count) == (long)sizeof count;
        long long after_read = now();
        /* What a read says is how many milliseconds had gone by when it was
           made. */
        check("one set for every millisecond counts every one",
              set && got && (long long)count >= (before_read - after_set) / MS
                  && (long long)count <= (after_read - before_set) / MS);

        struct itimerspec half = { { 0, 0 }, { 0, 500 * MS } }, is;
        timerfd_settime(fd, 0, &half, NULL);
        timerfd_gettime(fd, &is);
        check("what is left of it is said in nanoseconds",
              is.it_value.tv_sec == 0 && is.it_value.tv_nsec > 400 * MS && is.it_value.tv_nsec <= 500 * MS
                  && is.it_interval.tv_sec == 0 && is.it_interval.tv_nsec == 0);
        close(fd);
    }
    /* One on the clock that says the date, set for what that clock will
       read. */
    fd = timerfd_create(CLOCK_REALTIME, 0);
    if (fd >= 0) {
        unsigned long long count = 0;
        struct itimerspec at;
        memset(&at, 0, sizeof at);
        at.it_value = span(ns_on(CLOCK_REALTIME) + 5 * MS);
        long long from = now();
        int ok = timerfd_settime(fd, TFD_TIMER_ABSTIME, &at, NULL) == 0
                 && read(fd, &count, sizeof count) == (long)sizeof count && count == 1;
        long long took = now() - from;
        check("a timer set for a date fires at it", ok && took >= 5 * MS - 100000 && took < 1000 * MS);
        close(fd);
    } else {
        check("a timer on the clock that says the date", 0);
    }

    /* Setting the date: for a program that may. */
    struct timespec then = span(ns_on(CLOCK_REALTIME) + 100000LL * MS);
    long long boot = now();
    if (clock_settime(CLOCK_REALTIME, &then) == 0) {
        long long reads = ns_on(CLOCK_REALTIME);
        long long ahead = (long long)then.tv_sec * 1000000000LL + then.tv_nsec;
        check("the date is set, and time goes on from there", reads >= ahead && reads < ahead + 1000 * MS);
        check("the clock that counts from boot does not move with it", now() - boot < 1000 * MS);
        struct timespec before = { 0, 0 };
        check("a date before there were any is refused",
              clock_settime(CLOCK_REALTIME, &before) == -1 && errno == EINVAL);
        check("and so is setting a clock that is not the date",
              clock_settime(CLOCK_MONOTONIC, &then) == -1 && errno == EINVAL);
        /* Back, by the other call, to what it was and the time this took. */
        long long back = date + (now() - first);
        struct timeval was = { back / 1000000000LL, back % 1000000000LL / 1000 };
        check("and it is set back", settimeofday(&was, NULL) == 0
                                        && ns_on(CLOCK_REALTIME) - back < 1000 * MS
                                        && ns_on(CLOCK_REALTIME) >= back);
    } else {
        check("a program that may not set the clock is told so", errno == EPERM);
        printf("        this program does not hold the right to set the clock: setting it is not checked\n");
    }

    printf("clocktest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
