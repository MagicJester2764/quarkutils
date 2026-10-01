/* Waiting, and the clock a program measures the wait with.
 *
 * A main loop is `poll` with a timeout plus something to wake it early, and
 * every toolkit has one. Three things have to be true for that to work, and
 * each is checked here on its own: `poll` returns when its timeout runs out
 * even though nothing became ready, the monotonic clock advances while it
 * waits, and a descriptor written from another thread wakes the poll that is
 * waiting on it.
 *
 * glib picks `eventfd` for its wakeup when the C library has one and falls
 * back to a pipe when the call fails, so both are tested.
 */

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <time.h>
#include <unistd.h>

static int failures;

static void check(const char *what, int ok) {
    printf("%s: %s\n", ok ? "ok" : "FAILED", what);
    if (!ok) {
        failures++;
    }
}

static long long now_ms(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return -1;
    }
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int wake_fd;

static void *waker(void *arg) {
    (void)arg;
    struct timespec nap = {0, 150 * 1000 * 1000};
    nanosleep(&nap, NULL);
    uint64_t one = 1;
    ssize_t w = write(wake_fd, &one, sizeof one);
    (void)w;
    return NULL;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);

    /* The clock has to move, or every timeout in every main loop is for
       ever. */
    long long t0 = now_ms();
    struct timespec nap = {0, 120 * 1000 * 1000};
    nanosleep(&nap, NULL);
    long long t1 = now_ms();
    check("monotonic clock advances", t0 >= 0 && t1 - t0 >= 100 && t1 - t0 < 2000);

    /* A poll that nothing satisfies comes back when its time is up. */
    int p[2];
    check("pipe2", pipe2(p, O_CLOEXEC) == 0);
    struct pollfd pf = {.fd = p[0], .events = POLLIN};
    t0 = now_ms();
    int n = poll(&pf, 1, 200);
    t1 = now_ms();
    check("poll times out", n == 0 && t1 - t0 >= 150 && t1 - t0 < 2000);

    /* And when it is satisfied it comes back at once. */
    check("write to pipe", write(p[1], "x", 1) == 1);
    t0 = now_ms();
    n = poll(&pf, 1, 2000);
    t1 = now_ms();
    check("poll sees a ready pipe", n == 1 && (pf.revents & POLLIN) && t1 - t0 < 150);
    char c;
    check("read it back", read(p[0], &c, 1) == 1 && c == 'x');

    /* Waiting on nothing at all is a sleep, and some loops do exactly that
       when every source is a timeout. */
    t0 = now_ms();
    n = poll(NULL, 0, 150);
    t1 = now_ms();
    check("poll with no descriptors", n == 0 && t1 - t0 >= 100 && t1 - t0 < 2000);

    /* O_NONBLOCK has to mean it. A main loop drains its wakeup one byte at a
       time "until it is empty", and empty is a read that says EAGAIN rather
       than one that waits -- glib's does exactly that, holding its context
       lock, so a read that blocks is a program that stops. */
    check("set nonblocking", fcntl(p[0], F_SETFL, fcntl(p[0], F_GETFL) | O_NONBLOCK) == 0);
    check("empty pipe does not block", read(p[0], &c, 1) == -1 && errno == EAGAIN);
    check("write to pipe again", write(p[1], "yz", 2) == 2);
    check("one byte at a time", read(p[0], &c, 1) == 1 && c == 'y');
    check("then the other", read(p[0], &c, 1) == 1 && c == 'z');
    check("and then empty", read(p[0], &c, 1) == -1 && errno == EAGAIN);

    /* A pipe made non-blocking is the same thing, without the fcntl. */
    int q[2];
    check("pipe2 nonblocking", pipe2(q, O_CLOEXEC | O_NONBLOCK) == 0);
    check("a new nonblocking pipe is empty", read(q[0], &c, 1) == -1 && errno == EAGAIN);
    check("and takes a byte", write(q[1], "q", 1) == 1 && read(q[0], &c, 1) == 1 && c == 'q');
    close(q[0]);
    close(q[1]);

    /* eventfd: a counter that is readable when it is not zero. */
    int ev = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    check("eventfd", ev >= 0);
    if (ev >= 0) {
        uint64_t v = 0;
        check("empty eventfd does not read", read(ev, &v, sizeof v) == -1 && errno == EAGAIN);
        struct pollfd ef = {.fd = ev, .events = POLLIN};
        check("empty eventfd is not readable", poll(&ef, 1, 50) == 0);
        uint64_t add = 3;
        check("eventfd write", write(ev, &add, sizeof add) == (ssize_t)sizeof add);
        ef.revents = 0;
        check("eventfd becomes readable", poll(&ef, 1, 500) == 1 && (ef.revents & POLLIN));
        check("eventfd reads the sum", read(ev, &v, sizeof v) == (ssize_t)sizeof v && v == 3);
        check("and is empty again", read(ev, &v, sizeof v) == -1 && errno == EAGAIN);
    }

    /* The whole point: a thread wakes a poll that is already waiting. */
    wake_fd = ev >= 0 ? ev : p[1];
    int watch = ev >= 0 ? ev : p[0];
    pthread_t th;
    check("thread started", pthread_create(&th, NULL, waker, NULL) == 0);
    struct pollfd wf = {.fd = watch, .events = POLLIN};
    t0 = now_ms();
    n = poll(&wf, 1, 3000);
    t1 = now_ms();
    check("another thread wakes a waiting poll", n == 1 && t1 - t0 >= 100 && t1 - t0 < 2000);
    pthread_join(th, NULL);

    printf("polltest: %d failed\n", failures);
    return failures ? 1 : 0;
}
