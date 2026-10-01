// LINK: -lutil
/* A pseudo-terminal: the pair a terminal emulator runs a shell through.
 *
 * `openpty` is the whole of what a terminal asks for, and `forkpty` is that
 * plus a fork and three descriptors pointed at the slave. Both are here
 * because the second is what `weston-terminal` calls and the first is what it
 * is made of. */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <pty.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* Read with a bound on the waiting, so that a test of a thing that blocks
   cannot itself become a program that hangs. */
static int read_soon(int fd, char *buf, int max) {
    for (int i = 0; i < 200; i++) {
        struct pollfd p = { .fd = fd, .events = POLLIN };
        if (poll(&p, 1, 50) > 0) {
            int n = (int)read(fd, buf, max);
            return n;
        }
    }
    return -1;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("pty:\n");

    int master = -1, slave = -1;
    check("openpty", openpty(&master, &slave, NULL, NULL, NULL) == 0);
    if (master < 0) {
        printf("ptytest: %s\n", strerror(errno));
        return 1;
    }
    check("two descriptors of its own", master != slave && master >= 0 && slave >= 0);
    check("the slave is a terminal", isatty(slave));
    check("and so is the master", isatty(master));

    /* A terminal has a name, which is the path it is opened by: what `tty`
       prints, and what a program hands to another to say where to write. */
    char *called = ttyname(slave);
    char *pts = ptsname(master);
    check("the slave has a name", called != NULL);
    check("the one its master says it has", called && pts && !strcmp(called, pts));
    struct stat by_name, by_fd, by_copy;
    int copy = dup(slave);
    check("the name and the descriptor are one file",
          called && stat(called, &by_name) == 0 && fstat(slave, &by_fd) == 0
              && by_name.st_dev == by_fd.st_dev && by_name.st_ino == by_fd.st_ino
              && S_ISCHR(by_fd.st_mode));
    check("and so are two descriptors for it",
          copy >= 0 && fstat(copy, &by_copy) == 0 && by_copy.st_dev == by_fd.st_dev
              && by_copy.st_ino == by_fd.st_ino);
    close(copy);
    int ends[2];
    errno = 0;
    check("a pipe is not a terminal and has no such name",
          pipe(ends) == 0 && ttyname(ends[0]) == NULL && errno == ENOTTY);
    close(ends[0]);
    close(ends[1]);

    /* Typing: what is written to the master is what the program in the
       terminal reads. In canonical mode it arrives a line at a time. */
    char buf[128];
    write(master, "hello", 5);
    struct pollfd p = { .fd = slave, .events = POLLIN };
    check("half a line is not a line", poll(&p, 1, 50) == 0);
    write(master, "\n", 1);
    int n = read_soon(slave, buf, sizeof buf - 1);
    if (n > 0) {
        buf[n] = 0;
    }
    check("and the whole line arrives at once", n == 6 && !strcmp(buf, "hello\n"));

    /* Echo: the master sees what was typed, so that a terminal can draw it
       without the shell printing it back. */
    n = read_soon(master, buf, sizeof buf - 1);
    if (n > 0) {
        buf[n] = 0;
    }
    check("the typing came back to be drawn", n > 0 && !strncmp(buf, "hello", 5));

    /* Output: what the program writes comes out of the master, with the
       newline expanded so a terminal's cursor goes back to the left. */
    write(slave, "there\n", 6);
    n = read_soon(master, buf, sizeof buf - 1);
    if (n > 0) {
        buf[n] = 0;
    }
    check("output arrives with the return before the newline",
          n == 7 && !strcmp(buf, "there\r\n"));

    /* Raw mode: no echo, and a byte at a time. */
    struct termios t;
    check("its settings can be read", tcgetattr(slave, &t) == 0);
    t.c_lflag &= ~(ICANON | ECHO);
    check("and written", tcsetattr(slave, TCSANOW, &t) == 0);
    write(master, "x", 1);
    n = read_soon(slave, buf, sizeof buf - 1);
    check("a single byte arrives with no newline", n == 1 && buf[0] == 'x');
    p = (struct pollfd){ .fd = master, .events = POLLIN };
    check("and nothing was echoed", poll(&p, 1, 50) == 0);

    /* A window size, which a terminal sets and the program in it reads. */
    struct winsize ws = { .ws_row = 24, .ws_col = 80 };
    check("a window size can be set", ioctl(master, TIOCSWINSZ, &ws) == 0);
    struct winsize got = { 0 };
    check("and read back from the other end",
          ioctl(slave, TIOCGWINSZ, &got) == 0 && got.ws_row == 24 && got.ws_col == 80);

    close(slave);
    n = (int)read(master, buf, sizeof buf - 1);
    check("the master reads the end when the slave goes", n == 0);
    close(master);

    /* And the whole thing at once, which is what a terminal emulator calls. */
    int m2 = -1;
    pid_t pid = forkpty(&m2, NULL, NULL, NULL);
    if (pid == 0) {
        /* The slave is this program's standard input, output and error. */
        printf("from inside the terminal\n");
        fflush(stdout);
        _exit(7);
    }
    check("forkpty made a child", pid > 0);
    n = read_soon(m2, buf, sizeof buf - 1);
    if (n > 0) {
        buf[n] = 0;
    }
    check("what it printed came out of the master",
          n > 0 && strstr(buf, "from inside the terminal") != NULL);
    int status = 0;
    check("and it exited", waitpid(pid, &status, 0) == pid && WEXITSTATUS(status) == 7);
    close(m2);

    printf("ptytest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
