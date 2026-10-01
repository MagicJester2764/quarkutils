/* A named pipe: a pipe two programs find by a name in the filesystem rather
 * than by being handed its ends.
 *
 * The name, its permissions and its inode are the file server's. The pipe is
 * the kernel's, like any other, which is what lets a program `poll` it. What
 * joins the two is the open: the file server gives whoever opens the name an
 * end of the one pipe that name has, for as long as anybody has it open. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define NAME "/tmp/fifotest.pipe"

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static struct timespec started;

static void start(void) {
    clock_gettime(CLOCK_MONOTONIC, &started);
}

static long ms(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - started.tv_sec) * 1000 + (now.tv_nsec - started.tv_nsec) / 1000000;
}

static volatile sig_atomic_t rang;

static void ring(int sig) {
    (void)sig;
    rang++;
}

int main(void) {
    printf("named pipes:\n");
    struct stat st;
    int status = 0;
    char buf[64];

    unlink(NAME);
    check("mkfifo makes one", mkfifo(NAME, 0640) == 0);
    check("and it is there, a pipe, with the mode it was given",
          stat(NAME, &st) == 0 && S_ISFIFO(st.st_mode) && (st.st_mode & 0777) == (0640 & ~umask(0)));
    errno = 0;
    check("a second by the same name is refused", mkfifo(NAME, 0600) == -1 && errno == EEXIST);

    /* An open waits for the other end. The reader is here; the writer is a
       child that takes a fifth of a second to get round to it. */
    start();
    pid_t child = fork();
    if (child == 0) {
        usleep(200 * 1000);
        int w = open(NAME, O_WRONLY);
        if (w < 0) {
            _exit(2);
        }
        if (write(w, "through the name", 16) != 16) {
            _exit(3);
        }
        usleep(100 * 1000);
        close(w);
        _exit(0);
    }
    int r = open(NAME, O_RDONLY);
    long waited = ms();
    check("opening to read waits for somebody to open it to write", r >= 0 && waited >= 150);
    check("what was opened is a pipe", fstat(r, &st) == 0 && S_ISFIFO(st.st_mode));
    memset(buf, 0, sizeof buf);
    check("what the writer wrote arrives", read(r, buf, sizeof buf) == 16 && !strcmp(buf, "through the name"));
    check("and when the writer has gone, the end of it", read(r, buf, sizeof buf) == 0);
    check("the writer got on with it", waitpid(child, &status, 0) == child && WEXITSTATUS(status) == 0);

    /* The other way round: a writer waits for a reader. The read end above
       goes first — a child would inherit it, and be the reader nobody was
       supposed to have yet. */
    close(r);
    start();
    child = fork();
    if (child == 0) {
        usleep(200 * 1000);
        int fd = open(NAME, O_RDONLY);
        char got[8] = { 0 };
        _exit(fd >= 0 && read(fd, got, sizeof got) == 2 && !strcmp(got, "hi") ? 0 : 1);
    }
    int w = open(NAME, O_WRONLY);
    waited = ms();
    check("opening to write waits for somebody to open it to read", w >= 0 && waited >= 150);
    check("and writes to it", write(w, "hi", 2) == 2);
    close(w);
    check("which the reader read", waitpid(child, &status, 0) == child && WEXITSTATUS(status) == 0);

    /* Asked not to wait. */
    errno = 0;
    w = open(NAME, O_WRONLY | O_NONBLOCK);
    check("a writer that will not wait, with nobody reading, is refused", w == -1 && errno == ENXIO);
    start();
    r = open(NAME, O_RDONLY | O_NONBLOCK);
    check("a reader that will not wait is given its end at once", r >= 0 && ms() < 100);
    struct pollfd p = { .fd = r, .events = POLLIN };
    check("with nothing in it and nobody writing, a poll finds nothing to read yet",
          poll(&p, 1, 50) == 0 || (p.revents & POLLHUP));
    w = open(NAME, O_WRONLY | O_NONBLOCK);
    check("and now a writer that will not wait is let in", w >= 0);
    check("a poll sees what it writes", write(w, "x", 1) == 1 && poll(&p, 1, 1000) == 1 && (p.revents & POLLIN));
    check("and it is read", read(r, buf, 1) == 1 && buf[0] == 'x');

    /* Nobody reading: the writer is told the way any pipe's is. */
    signal(SIGPIPE, SIG_IGN);
    close(r);
    errno = 0;
    check("a write with no reader left is a broken pipe", write(w, "y", 1) == -1 && errno == EPIPE);
    close(w);

    /* What was in it goes when the last end does: the name is a place to
       meet, not somewhere to leave things. */
    r = open(NAME, O_RDONLY | O_NONBLOCK);
    w = open(NAME, O_WRONLY | O_NONBLOCK);
    check("both ends again", r >= 0 && w >= 0 && write(w, "left behind", 11) == 11);
    close(r);
    close(w);
    r = open(NAME, O_RDONLY | O_NONBLOCK);
    w = open(NAME, O_WRONLY | O_NONBLOCK);
    errno = 0;
    check("what was left in it when everybody closed is gone",
          r >= 0 && w >= 0 && read(r, buf, sizeof buf) == -1 && errno == EAGAIN);
    close(r);
    close(w);

    /* An open that is waiting is a wait like any other: a signal ends it. */
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = ring;
    sigaction(SIGALRM, &sa, 0);
    struct itimerval fifth = { .it_value = { .tv_usec = 200 * 1000 } };
    start();
    setitimer(ITIMER_REAL, &fifth, 0);
    errno = 0;
    r = open(NAME, O_RDONLY);
    check("a signal ends an open that is waiting", r == -1 && errno == EINTR && rang == 1 && ms() >= 150);
    errno = 0;
    check("and the open that was given up left no reader behind",
          open(NAME, O_WRONLY | O_NONBLOCK) == -1 && errno == ENXIO);

    /* Unless the handler asked for what it interrupted to go on. */
    sa.sa_flags = SA_RESTART;
    sigaction(SIGALRM, &sa, 0);
    start();
    child = fork();
    if (child == 0) {
        usleep(400 * 1000);
        _exit(open(NAME, O_WRONLY) >= 0 ? 0 : 1);
    }
    setitimer(ITIMER_REAL, &fifth, 0);
    r = open(NAME, O_RDONLY);
    waited = ms();
    check("one whose handler asks for restarting goes on waiting, and is opened",
          r >= 0 && rang == 2 && waited >= 350);
    close(r);
    check("by the writer it waited for", waitpid(child, &status, 0) == child && WEXITSTATUS(status) == 0);

    /* What F_GETFL says of an end: which end. */
    r = open(NAME, O_RDONLY | O_NONBLOCK);
    w = open(NAME, O_WRONLY);
    check("each end says which it is",
          (fcntl(r, F_GETFL) & O_ACCMODE) == O_RDONLY && (fcntl(w, F_GETFL) & O_ACCMODE) == O_WRONLY &&
              (fcntl(r, F_GETFL) & O_NONBLOCK) && !(fcntl(w, F_GETFL) & O_NONBLOCK));
    close(r);
    close(w);

    check("mknod makes one too", mknod(NAME "2", S_IFIFO | 0600, 0) == 0 &&
                                     stat(NAME "2", &st) == 0 && S_ISFIFO(st.st_mode));
    errno = 0;
    check("and will not make a device",
          mknod(NAME "3", S_IFCHR | 0600, makedev(1, 3)) == -1 && errno == EPERM);
    unlink(NAME "2");
    check("unlink removes the name", unlink(NAME) == 0 && stat(NAME, &st) == -1);

    printf("fifotest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
