/* A local pair of messages, and descriptors that close at an exec.
 *
 * socketpair of SOCK_SEQPACKET keeps each write whole for a read to take
 * whole, and drops what does not fit a reader's buffer: Rust's standard
 * library makes one whenever it forks to start a program, and reads it to
 * hear how the exec went. It was refused, as not implemented, and so was
 * every such start — cargo's of rustc among them.
 *
 * And SOCK_CLOEXEC and pipe2's O_CLOEXEC mark what they make to close when
 * the program becomes another. pipe2 did not, and musl's posix_spawn, which
 * hears how its child's exec went by reading a pipe the exec closes, waited
 * for every program it started to end before it returned.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

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

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "sleep") == 0) {
        sleep(2);
        return 0;
    }

    int sv[2];
    check("a pair of SOCK_SEQPACKET is made",
          socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv) == 0);
    char buf[64];
    check("two writes are two messages",
          write(sv[0], "one", 3) == 3 && write(sv[0], "three", 5) == 5 &&
          read(sv[1], buf, sizeof buf) == 3 && read(sv[1], buf, sizeof buf) == 5 &&
          memcmp(buf, "three", 5) == 0);
    check("what does not fit a read is dropped, and the next message is whole",
          write(sv[0], "0123456789", 10) == 10 && write(sv[0], "next", 4) == 4 &&
          read(sv[1], buf, 4) == 4 && memcmp(buf, "0123", 4) == 0 &&
          read(sv[1], buf, sizeof buf) == 4 && memcmp(buf, "next", 4) == 0);
    check("both ends close at an exec (SOCK_CLOEXEC)",
          (fcntl(sv[0], F_GETFD) & FD_CLOEXEC) && (fcntl(sv[1], F_GETFD) & FD_CLOEXEC));
    close(sv[0]);
    check("a read after the other end has gone is the end", read(sv[1], buf, sizeof buf) == 0);
    close(sv[1]);

    int p[2];
    check("pipe2(O_CLOEXEC) marks both ends to close at an exec",
          pipe2(p, O_CLOEXEC) == 0 && (fcntl(p[0], F_GETFD) & FD_CLOEXEC) &&
          (fcntl(p[1], F_GETFD) & FD_CLOEXEC));
    close(p[0]);
    close(p[1]);

    /* By its path: run from a shell, argv[0] is the name it was typed as. */
    char self[256];
    if (strchr(argv[0], '/'))
        snprintf(self, sizeof self, "%s", argv[0]);
    else
        snprintf(self, sizeof self, "/usr/bin/%s", argv[0]);
    char *child_argv[] = { self, "sleep", NULL };
    pid_t pid = -1;
    double before = now();
    int spawned = posix_spawn(&pid, self, NULL, NULL, child_argv, environ) == 0;
    double took = now() - before;
    check("posix_spawn returns while the program it started runs", spawned && took < 1.5);
    int status = -1;
    check("and the program runs to its end",
          spawned && waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);

    printf("packettest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
