/* Waiting on many descriptors at once: as many as a program has, where it
 * was thirty-two. A desktop's main loop polls its display connection, its
 * session bus, its timers and every client it serves; glib builds one array
 * of all of them for every turn of the loop.
 *
 * poll of 200, select of 100 and an epoll set watching 500, each with one
 * of them ready and that one the one reported; and a poll of more than the
 * program may have descriptors refused, as Linux refuses it.
 */

#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/select.h>
#include <unistd.h>

static int failures;

static void check(const char *what, int ok) {
    printf("%s: %s\n", ok ? "ok" : "FAILED", what);
    if (!ok) {
        failures++;
    }
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);

    /* Two hundred pipes, the read ends polled and one of them written. */
    static int pipes[200][2];
    int made = 0;
    while (made < 200 && pipe(pipes[made]) == 0) {
        made++;
    }
    check("two hundred pipes", made == 200);
    static struct pollfd pf[200];
    for (int i = 0; i < made; i++) {
        pf[i].fd = pipes[i][0];
        pf[i].events = POLLIN;
        pf[i].revents = 0;
    }
    check("one of them written", made == 200 && write(pipes[150][1], "x", 1) == 1);
    int n = poll(pf, (nfds_t)made, 1000);
    int only = n == 1;
    for (int i = 0; i < made; i++) {
        only = only && ((pf[i].revents & POLLIN) != 0) == (i == 150);
    }
    check("poll of 200 says which", only);
    char c;
    check("and it reads", made == 200 && read(pipes[150][0], &c, 1) == 1 && c == 'x');

    /* select of a hundred of them. */
    fd_set rd;
    FD_ZERO(&rd);
    int top = 0;
    for (int i = 0; i < 100 && i < made; i++) {
        FD_SET(pipes[i][0], &rd);
        if (pipes[i][0] > top) {
            top = pipes[i][0];
        }
    }
    check("another written", made == 200 && write(pipes[42][1], "y", 1) == 1);
    struct timeval tv = {1, 0};
    n = select(top + 1, &rd, NULL, NULL, &tv);
    only = n == 1;
    for (int i = 0; i < 100 && i < made; i++) {
        only = only && (FD_ISSET(pipes[i][0], &rd) != 0) == (i == 42);
    }
    check("select of 100 says which", only);
    check("and it reads", made == 200 && read(pipes[42][0], &c, 1) == 1 && c == 'y');

    /* An epoll set watching five hundred counters. */
    static int ev[500];
    int counters = 0;
    while (counters < 500 && (ev[counters] = eventfd(0, EFD_NONBLOCK)) >= 0) {
        counters++;
    }
    int ep = epoll_create1(0);
    int watched = 0;
    for (int i = 0; i < counters; i++) {
        struct epoll_event e = {.events = EPOLLIN, .data.u32 = (uint32_t)i};
        if (epoll_ctl(ep, EPOLL_CTL_ADD, ev[i], &e) != 0) {
            break;
        }
        watched++;
    }
    check("an epoll set watches 500", counters == 500 && ep >= 0 && watched == 500);
    uint64_t one = 1;
    check("one of them added to", counters == 500 && write(ev[377], &one, sizeof one) == (ssize_t)sizeof one);
    struct epoll_event got[64];
    n = epoll_wait(ep, got, 64, 1000);
    check("epoll says which", n == 1 && got[0].data.u32 == 377 && (got[0].events & EPOLLIN));

    /* More than the program may have is refused, before any wait. */
    static struct pollfd too_many[2000];
    for (int i = 0; i < 2000; i++) {
        too_many[i].fd = pipes[0][0];
        too_many[i].events = POLLIN;
    }
    errno = 0;
    check("poll of more than its limit is refused", poll(too_many, 2000, 0) == -1 && errno == EINVAL);

    printf("pollmany: %d failed\n", failures);
    return failures ? 1 : 0;
}
