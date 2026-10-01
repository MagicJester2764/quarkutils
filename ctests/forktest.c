/* A process making another one: fork, and waiting for what it did.
 *
 * Every Unix program that starts a program does this, and a great many that do
 * not still fork to do one thing and get out of the way. A system without it
 * can run programs somebody wrote for it and nothing else. */
#include <errno.h>
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

int main(void) {
    printf("fork:\n");
    pid_t self = getpid();
    check("a pid of its own", self > 0);

    /* The child's half of what the parent can see: a variable written before
       the fork is the child's to change, and the parent must not see it. */
    volatile int shared = 1;

    pid_t pid = fork();
    if (pid == 0) {
        shared = 2;
        /* A child is a task of its own, so its pid is not its parent's. */
        _exit(getpid() == self ? 40 : 41);
    }
    check("fork returned a child", pid > 0);
    if (pid < 0) {
        printf("forktest: %d failed, %s\n", ++failed, strerror(errno));
        return 1;
    }

    int status = 0;
    pid_t got = waitpid(pid, &status, 0);
    check("waitpid returned the child", got == pid);
    check("it exited", WIFEXITED(status));
    check("with its own pid", WEXITSTATUS(status) == 41);
    check("and the parent's memory is the parent's", shared == 1);

    /* Again, with the child doing a little more than exiting: a fork whose
       child writes down a pipe is the shape every program uses. */
    int fds[2];
    check("a pipe", pipe(fds) == 0);
    pid = fork();
    if (pid == 0) {
        close(fds[0]);
        write(fds[1], "from the child", 14);
        close(fds[1]);
        _exit(0);
    }
    close(fds[1]);
    char buf[32] = { 0 };
    int n = (int)read(fds[0], buf, sizeof buf - 1);
    close(fds[0]);
    check("the child wrote down it", n == 14 && !strcmp(buf, "from the child"));
    check("and was waited for", waitpid(pid, &status, 0) == pid && WEXITSTATUS(status) == 0);

    printf("forktest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
