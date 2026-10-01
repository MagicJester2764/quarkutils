/* Becoming another program, which is the other half of starting one.
 *
 * `fork` alone makes a copy of what is already running; a program that starts
 * a *different* program does it by forking and then execing, and a terminal
 * emulator is exactly that shape. This checks both ends: that a failed exec
 * leaves the caller running, and that a successful one keeps the things the
 * caller had — its process id, and the descriptors it was given. */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

/* Where the image is staged, which is what this program execs to become
   itself again. */
#define SELF "/usr/bin/exectest"

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

int main(int argc, char **argv) {
    /* The child half: this program re-executed, saying so and exiting. */
    if (argc > 1 && !strcmp(argv[1], "--execed")) {
        setvbuf(stdout, NULL, _IONBF, 0);
        printf("  exec'd, pid %d, argc %d\n", (int)getpid(), argc);
        /* Descriptor 3 is a pipe the parent left open across the exec: the
           kernel's descriptors belong to the task, and an exec keeps the
           task. */
        int n = (int)write(3, "through the exec", 16);
        _exit(n == 16 ? 17 : 18);
    }

    /* Unbuffered: this program replaces itself, and a buffer is the one thing
       an exec does not carry across. Anything printed and not yet written
       would be lost with the image that held it. */
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("exec:\n");
    pid_t self = getpid();

    /* A failure must leave the caller exactly as it was. */
    char *nothing[] = { (char *)"/usr/bin/there-is-no-such-program", NULL };
    check("exec of a path that is not there fails",
          execv(nothing[0], nothing) == -1 && errno != 0);
    check("and the caller is still running", getpid() == self);

    char *notelf[] = { (char *)"/etc/libc.tests", NULL };
    check("exec of something that is not a program fails",
          execv(notelf[0], notelf) == -1);
    check("and the caller is still here too", getpid() == self);

    int fds[2];
    check("a pipe to hear the new program through", pipe(fds) == 0);

    pid_t pid = fork();
    if (pid == 0) {
        /* The read end is the parent's; the write end becomes descriptor 3,
           which the exec must carry across. */
        close(fds[0]);
        if (fds[1] != 3) {
            dup2(fds[1], 3);
            close(fds[1]);
        }
        /* By its path, not by `argv[0]`: a forked child has no working
           directory of its own here — the server keeps one per program and
           the child is a new program — so a relative name resolves against
           the root and finds nothing. */
        char *args[] = { (char *)SELF, (char *)"--execed", NULL };
        execv(SELF, args);
        _exit(19);
    }
    close(fds[1]);
    char buf[32] = { 0 };
    int n = (int)read(fds[0], buf, sizeof buf - 1);
    close(fds[0]);
    check("the new program wrote down the descriptor it inherited",
          n == 16 && !strcmp(buf, "through the exec"));

    int status = 0;
    check("and exited as itself", waitpid(pid, &status, 0) == pid &&
                                      WIFEXITED(status) && WEXITSTATUS(status) == 17);

    printf("exectest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
