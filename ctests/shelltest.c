// LINK: -lutil
/* A shell in a terminal, without the terminal.
 *
 * This is what `weston-terminal` does — a pty, a fork, a shell on the other
 * end — with the drawing left out, so that when the terminal does not work it
 * is clear which half is at fault. */
#define _GNU_SOURCE
#include <poll.h>
#include <pty.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* Read whatever arrives in the next second or so, appending to `buf`. */
static int drain(int fd, char *buf, int max) {
    int got = 0;
    for (int i = 0; i < 100 && got < max - 1; i++) {
        struct pollfd p = { .fd = fd, .events = POLLIN };
        if (poll(&p, 1, 50) <= 0) {
            continue;
        }
        int n = (int)read(fd, buf + got, max - 1 - got);
        if (n <= 0) {
            break;
        }
        got += n;
        buf[got] = 0;
    }
    return got;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("a shell in a terminal:\n");

    int master = -1;
    pid_t pid = forkpty(&master, NULL, NULL, NULL);
    if (pid == 0) {
        char *args[] = { (char *)"/bin/sh", NULL };
        execv(args[0], args);
        _exit(9);
    }
    check("forkpty started something", pid > 0);

    char buf[2048] = { 0 };
    int n = drain(master, buf, sizeof buf);
    check("the shell printed a prompt", n > 0 && strchr(buf, '$') != NULL);
    printf("  [it said %d bytes: %.200s]\n", n, buf);

    /* A command, typed as a person would type it. */
    write(master, "echo hello from the shell\n", 26);
    memset(buf, 0, sizeof buf);
    n = drain(master, buf, sizeof buf);
    check("and answered a command",
          n > 0 && strstr(buf, "hello from the shell") != NULL);

    /* A program of its own, started by the shell inside the terminal. */
    write(master, "ls /\n", 5);
    memset(buf, 0, sizeof buf);
    n = drain(master, buf, sizeof buf);
    check("and ran a program that printed", n > 0 && strstr(buf, "usr") != NULL);
    printf("  [ls said %d bytes: %.300s]\n", n, buf);

    /* Ending it: the shell exits, the master reads the end of file. */
    write(master, "exit\n", 5);
    memset(buf, 0, sizeof buf);
    drain(master, buf, sizeof buf);
    int status = 0;
    check("the shell exited", waitpid(pid, &status, 0) == pid);
    close(master);

    printf("shelltest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
