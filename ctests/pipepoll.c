/* A poll is woken by a write that fills what it waits on.
 *
 * A pipe here holds 4 KiB. A write of more puts in what fits and waits for
 * room — and a reader waiting in poll was told of none of it until the
 * whole write was over, which it never was, since the reader was not
 * reading. cargo reads what rustc prints by polling: rustc printed more
 * than 4 KiB at once, and the two waited for each other for good.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static double now(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int to;
static char out[8192];

static void *writer(void *unused)
{
    (void)unused;
    usleep(100000);
    return (void *)(long)write(to, out, sizeof out);
}

/* Wait in poll for `from` while a thread writes 8 KiB to `into` in one
   call; then read it all. */
static void across(const char *what, int from, int into)
{
    char said[160];
    to = into;
    memset(out, 'q', sizeof out);
    pthread_t t;
    pthread_create(&t, 0, writer, 0);
    struct pollfd p = { .fd = from, .events = POLLIN };
    double began = now();
    int r = poll(&p, 1, 3000);
    double took = now() - began;
    snprintf(said, sizeof said, "a poll on %s is woken by a write that fills it and waits for room", what);
    check(said, r == 1 && (p.revents & POLLIN) && took < 2.0);
    char in[8192];
    size_t got = 0;
    while (got < sizeof in) {
        ssize_t n = read(from, in + got, sizeof in - got);
        if (n <= 0)
            break;
        got += n;
    }
    void *wrote;
    pthread_join(t, &wrote);
    snprintf(said, sizeof said, "and the whole write goes through %s", what);
    check(said, got == sizeof in && (long)wrote == (long)sizeof out);
}

int main(void)
{
    int p[2];
    if (pipe(p) == 0) {
        across("a pipe", p[0], p[1]);
        close(p[0]);
        close(p[1]);
    }
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0) {
        across("a stream", sv[0], sv[1]);
        close(sv[0]);
        close(sv[1]);
    }
    printf("pipepoll: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
