/* A deadline as a descriptor.
 *
 * A toolkit's event loop already waits on descriptors — a display connection,
 * a terminal, a pipe — and a cursor that blinks needs the same wait to end at
 * a time rather than at an event. That is what a timerfd is for, and weston's
 * own toytoolkit makes one for exactly that. */
#define _GNU_SOURCE
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static long ms_now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("timers:\n");

    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    check("a timer descriptor", fd >= 0);
    if (fd < 0) {
        printf("timertest: FAILED\n");
        return 1;
    }

    /* Two hundred milliseconds, once. */
    struct itimerspec it = { { 0, 0 }, { 0, 200 * 1000000L } };
    check("it can be armed", timerfd_settime(fd, 0, &it, NULL) == 0);
    struct pollfd p = { .fd = fd, .events = POLLIN };
    check("and is not ready before its time", poll(&p, 1, 50) == 0);
    long began = ms_now();
    check("the wait ends when it fires", poll(&p, 1, 2000) == 1);
    long waited = ms_now() - began;
    check("at about the time asked for", waited >= 100 && waited < 600);
    unsigned long long count = 0;
    check("and reads as one expiration",
          read(fd, &count, sizeof count) == (long)sizeof count && count == 1);

    /* Repeating: every hundred milliseconds. */
    struct itimerspec every = { { 0, 100 * 1000000L }, { 0, 100 * 1000000L } };
    check("a repeating timer", timerfd_settime(fd, 0, &every, NULL) == 0);
    check("fires again", poll(&p, 1, 2000) == 1 &&
                            read(fd, &count, sizeof count) == (long)sizeof count && count >= 1);
    /* Left alone for a while, the expirations are counted rather than lost. */
    usleep(350 * 1000);
    check("and counts what a slow reader missed",
          read(fd, &count, sizeof count) == (long)sizeof count && count >= 2);

    struct itimerspec off = { { 0, 0 }, { 0, 0 } };
    check("it can be disarmed", timerfd_settime(fd, 0, &off, NULL) == 0);
    check("and then never fires", poll(&p, 1, 200) == 0);
    close(fd);

    printf("timertest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
