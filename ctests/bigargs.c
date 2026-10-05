/* A program started with a great deal of environment gets all of it.
 *
 * What a program finds on its stack when it starts — its arguments, its
 * environment — was laid out on one page, and what did not fit was left
 * off the end without a word: cargo gives rustc kilobytes of environment,
 * rustc gives the linker that and a long command line, and cc, started
 * without most of its environment, could not find where it was installed.
 * It may take 128 KiB now, and more is refused (E2BIG), as on Linux.
 *
 *   bigargs                      the checks
 *   bigargs child ARGS ENVS      exit 0 if started with ARGS more arguments
 *                                and ENVS variables, every one as it was made
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

/* Argument `i`, and variable `i` of the environment, as they are made. */
static void arg(char *out, int i)
{
    snprintf(out, 32, "argument-%04d-xxxxxxx", i);
}

static void var(char *out, int i)
{
    snprintf(out, 128, "BIGARGS_%04d=%0*d", i, 100, i);
}

static int child(int argc, char **argv)
{
    int args = atoi(argv[2]), envs = atoi(argv[3]);
    if (argc != 4 + args)
        return 1;
    char want[128];
    for (int i = 0; i < args; i++) {
        arg(want, i);
        if (strcmp(argv[4 + i], want) != 0)
            return 2;
    }
    int seen = 0;
    for (char **e = environ; *e; e++) {
        if (strncmp(*e, "BIGARGS_", 8) == 0) {
            var(want, atoi(*e + 8));
            if (strcmp(*e, want) != 0)
                return 3;
            seen++;
        }
    }
    return seen == envs ? 0 : 4;
}

static char self[256];

/* Start this program again as a child of `args` arguments and `envs`
   variables — and one more of `huge` bytes if that is not nought — and say
   how it ended: its status, or 100 + errno where the exec failed. */
static int start(int args, int envs, int huge)
{
    char **argv = calloc(args + 5, sizeof *argv);
    char **envp = calloc(envs + 2, sizeof *envp);
    char counts[2][16];
    snprintf(counts[0], sizeof counts[0], "%d", args);
    snprintf(counts[1], sizeof counts[1], "%d", envs);
    argv[0] = self;
    argv[1] = "child";
    argv[2] = counts[0];
    argv[3] = counts[1];
    for (int i = 0; i < args; i++) {
        argv[4 + i] = malloc(32);
        arg(argv[4 + i], i);
    }
    for (int i = 0; i < envs; i++) {
        envp[i] = malloc(128);
        var(envp[i], i);
    }
    if (huge) {
        envp[envs] = malloc(huge + 1);
        memset(envp[envs], 'h', huge);
        memcpy(envp[envs], "HUGE=", 5);
        envp[envs][huge] = 0;
    }
    pid_t pid = fork();
    if (pid == 0) {
        execve(self, argv, envp);
        _exit(100 + errno);
    }
    int status = -1;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

int main(int argc, char **argv)
{
    if (argc >= 4 && strcmp(argv[1], "child") == 0)
        return child(argc, argv);
    if (strchr(argv[0], '/'))
        snprintf(self, sizeof self, "%s", argv[0]);
    else
        snprintf(self, sizeof self, "/usr/bin/%s", argv[0]);

    check("a program started with 400 arguments and 40 KiB of environment gets every one",
          start(400, 400, 0) == 0);
    /* One string of 140 KiB: more than Linux allows one string, and more
       than Quark allows all of them. */
    check("and one started with more than 128 KiB is refused, E2BIG", start(0, 0, 140 * 1024) == 100 + E2BIG);
    printf("bigargs: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
