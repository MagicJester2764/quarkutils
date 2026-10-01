/* select, which is how a line editor waits for a key.
 *
 * readline does not `read` and block: it asks whether there is anything to
 * read, with `select` or `pselect`, and reads when there is. With no `select`
 * it falls back to something slower and stranger, so this is the one wait a
 * shell's prompt is made of.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/select.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("select:\n");
    int p[2];
    if (pipe(p) != 0) {
        check("a pipe", 0);
        return 1;
    }

    fd_set rd, wr;
    struct timeval none = {0, 0};
    FD_ZERO(&rd);
    FD_SET(p[0], &rd);
    check("an empty pipe is not readable", select(p[0] + 1, &rd, NULL, NULL, &none) == 0);
    check("and its bit is cleared", !FD_ISSET(p[0], &rd));

    FD_ZERO(&wr);
    FD_SET(p[1], &wr);
    check("its other end is writable", select(p[1] + 1, NULL, &wr, NULL, &none) == 1 && FD_ISSET(p[1], &wr));

    write(p[1], "x", 1);
    FD_ZERO(&rd);
    FD_SET(p[0], &rd);
    FD_ZERO(&wr);
    FD_SET(p[1], &wr);
    check("with a byte in it, both are ready",
          select(p[1] + 1, &rd, &wr, NULL, NULL) == 2 && FD_ISSET(p[0], &rd) && FD_ISSET(p[1], &wr));

    /* A wait with nothing to wait for is a sleep. */
    char c;
    read(p[0], &c, 1);
    struct timespec a, b;
    clock_gettime(CLOCK_MONOTONIC, &a);
    struct timeval tenth = {0, 100000};
    FD_ZERO(&rd);
    FD_SET(p[0], &rd);
    int n = select(p[0] + 1, &rd, NULL, NULL, &tenth);
    clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    check("a timeout is waited out", n == 0 && ms >= 90 && ms < 1000);

    /* The writer going is something to read: the end. */
    close(p[1]);
    FD_ZERO(&rd);
    FD_SET(p[0], &rd);
    check("a pipe whose writer has gone is readable",
          select(p[0] + 1, &rd, NULL, NULL, NULL) == 1 && read(p[0], &c, 1) == 0);

    /* pselect is the same wait with a finer clock. */
    struct timespec soon = {0, 50000000};
    int q[2];
    pipe(q);
    FD_ZERO(&rd);
    FD_SET(q[0], &rd);
    check("pselect times out too", pselect(q[0] + 1, &rd, NULL, NULL, &soon, NULL) == 0);

    /* A file is always ready, and a descriptor that is not one is an error. */
    int fd = open("/etc/passwd", O_RDONLY);
    FD_ZERO(&rd);
    FD_SET(fd, &rd);
    check("a file is always readable", fd >= 0 && select(fd + 1, &rd, NULL, NULL, &none) == 1);
    close(fd);
    FD_ZERO(&rd);
    FD_SET(fd, &rd);
    check("a closed descriptor is refused", select(fd + 1, &rd, NULL, NULL, &none) == -1 && errno == EBADF);

    printf("seltest: %d failed\n", failed);
    return failed ? 1 : 0;
}
