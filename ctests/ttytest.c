// LINK: -lutil
/* The line discipline, and a terminal that is full.
 *
 * `ptytest` is the pair of descriptors. This is what is between them: the
 * characters a line is edited with, the one that ends a file, the one that
 * interrupts — and what happens to a program that prints faster than its
 * terminal draws. A shell's terminal is all of these at once, and the last
 * was the one nothing had asked about: a write that did not fit returned 0,
 * which is what a full disk looks like, and `cat` of a long file said so. */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <pty.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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

/* Read with a bound on the waiting: a test of a thing that blocks must not
   itself become a program that hangs. -1 if nothing came. */
static int read_soon(int fd, char *buf, int max) {
    struct pollfd p = { .fd = fd, .events = POLLIN };
    if (poll(&p, 1, 2000) <= 0) {
        return -1;
    }
    return (int)read(fd, buf, max);
}

/* Type `keys` and read the line they make. */
static int typed(int master, int slave, const char *keys, char *line, int max) {
    write(master, keys, strlen(keys));
    int n = read_soon(slave, line, max - 1);
    line[n > 0 ? n : 0] = 0;
    return n;
}

/* Throw away what the terminal echoed. */
static void drain(int master) {
    char junk[512];
    struct pollfd p = { .fd = master, .events = POLLIN };
    while (poll(&p, 1, 20) > 0 && read(master, junk, sizeof junk) > 0) {
    }
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("line discipline:\n");

    int master = -1, slave = -1;
    if (openpty(&master, &slave, NULL, NULL, NULL) != 0) {
        printf("ttytest: openpty: %s\n", strerror(errno));
        return 1;
    }
    char line[256];

    check("erase takes a character back",
          typed(master, slave, "abc\177d\n", line, sizeof line) == 4 && !strcmp(line, "abd\n"));
    check("kill takes the line back",
          typed(master, slave, "junk\025keep\n", line, sizeof line) == 5 && !strcmp(line, "keep\n"));
    check("word erase takes a word back",
          typed(master, slave, "one two\027three\n", line, sizeof line) == 10
              && !strcmp(line, "one three\n"));
    drain(master);

    /* End of file. After something, it hands that over with no newline;
       alone, it is a read of nothing — once, and then the terminal is a
       terminal again. */
    check("end of file after text delivers the text",
          typed(master, slave, "abc\004", line, sizeof line) == 3 && !strcmp(line, "abc"));
    write(master, "\004", 1);
    struct pollfd p = { .fd = slave, .events = POLLIN };
    int ready = poll(&p, 1, 2000);
    check("end of file alone makes the terminal readable", ready == 1 && (p.revents & POLLIN));
    check("and that is not a hangup", !(p.revents & POLLHUP));
    check("it reads as nothing", read(slave, line, sizeof line) == 0);
    check("and the next line is a line",
          typed(master, slave, "next\n", line, sizeof line) == 5 && !strcmp(line, "next\n"));
    drain(master);

    /* The interrupt character is not input. It takes the line it was typed
       into with it, and shows as ^C. Ignored here, because this program holds
       the terminal it is typing at. */
    signal(SIGINT, SIG_IGN);
    write(master, "partial\003", 8);
    char echo[64];
    int n = read_soon(master, echo, sizeof echo - 1);
    echo[n > 0 ? n : 0] = 0;
    check("the interrupt character is shown as ^C", n > 0 && strstr(echo, "^C") != NULL);
    check("and the line it interrupted is gone",
          typed(master, slave, "ok\n", line, sizeof line) == 3 && !strcmp(line, "ok\n"));
    signal(SIGINT, SIG_DFL);
    drain(master);

    /* With the editing off, every one of those is a byte like any other. */
    struct termios t;
    tcgetattr(slave, &t);
    struct termios cooked = t;
    t.c_lflag &= ~(ICANON | ECHO | ISIG);
    tcsetattr(slave, TCSANOW, &t);
    write(master, "\177\025\027\004\003", 5);
    n = read_soon(slave, line, sizeof line);
    check("raw, the editing characters are bytes",
          n == 5 && !memcmp(line, "\177\025\027\004\003", 5));

    /* A program that prints more than the terminal holds waits for it to be
       read: every byte arrives, in order, and every write says it wrote
       everything. */
    printf("a full terminal:\n");
    t.c_oflag &= ~OPOST;
    tcsetattr(slave, TCSANOW, &t);
    enum { TOTAL = 20000, CHUNK = 1000 };
    pid_t pid = fork();
    if (pid == 0) {
        close(master);
        char block[CHUNK];
        for (int sent = 0; sent < TOTAL; sent += CHUNK) {
            for (int i = 0; i < CHUNK; i++) {
                block[i] = (char)('a' + (sent + i) % 26);
            }
            if (write(slave, block, CHUNK) != CHUNK) {
                _exit(1);
            }
        }
        _exit(0);
    }
    int got = 0, in_order = 1;
    while (got < TOTAL) {
        char block[300];
        n = read_soon(master, block, sizeof block);
        if (n <= 0) {
            break;
        }
        for (int i = 0; i < n; i++) {
            if (block[i] != (char)('a' + (got + i) % 26)) {
                in_order = 0;
            }
        }
        got += n;
        /* Slower than it is written, so that it fills. */
        if (got % 3000 < 300) {
            usleep(20000);
        }
    }
    check("every byte of a long write arrives", got == TOTAL);
    check("in order", in_order);
    int status = -1;
    check("and each write wrote all it was given",
          waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    tcsetattr(slave, TCSANOW, &cooked);

    /* With nobody left to draw it, a write fails; it does not wait for
       ever, and it does not pretend. */
    pid = fork();
    if (pid == 0) {
        close(master);
        /* Wait for the parent to let go of its end. */
        char c;
        read(slave, &c, 1);
        char block[1024];
        memset(block, 'x', sizeof block);
        for (int i = 0; i < 64; i++) {
            if (write(slave, block, sizeof block) < 0) {
                _exit(0);
            }
        }
        _exit(1);
    }
    close(slave);
    usleep(50000);
    close(master);
    check("a write to a terminal nobody is drawing fails",
          waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);

    printf("ttytest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
