/* posix_spawn, and vfork: starting a program without being a copy of the one
 * that starts it for longer than it takes.
 *
 * musl makes posix_spawn out of a child that borrows its parent's memory
 * (`clone` with CLONE_VM | CLONE_VFORK) and reports a failed exec down a
 * pipe. GNU make starts every command this way. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* What this program does when it is what was spawned: says so, and where it
   is, on its standard output, and exits with a status of its own. */
static int spawned(const char *how) {
    if (!strcmp(how, "group")) {
        printf("%s\n", getpgrp() == getpid() ? "leads" : "follows");
    } else {
        printf("spawned\n");
    }
    fflush(stdout);
    return 7;
}

/* Read everything from `fd` into `buf`, as a string. */
static void slurp(int fd, char *buf, size_t size) {
    size_t n = 0;
    ssize_t got;
    while (n + 1 < size && (got = read(fd, buf + n, size - 1 - n)) > 0) {
        n += (size_t)got;
    }
    buf[n] = 0;
}

int main(int argc, char **argv) {
    if (argc > 2 && !strcmp(argv[1], "spawned")) {
        return spawned(argv[2]);
    }
    printf("spawning:\n");

    /* This program, by a path: where it was run from, or where a test is
       installed. */
    char self[256];
    if (strchr(argv[0], '/')) {
        snprintf(self, sizeof self, "%s", argv[0]);
    } else {
        snprintf(self, sizeof self, "/usr/bin/%s", argv[0]);
    }

    int p[2];
    char said[64];
    int status = 0;
    pid_t pid = 0;
    posix_spawn_file_actions_t fa;
    char *plain[] = { "spawntest", "spawned", "plain", NULL };

    /* A child with its standard output put on a pipe by the actions. */
    check("a pipe", pipe(p) == 0);
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, p[1], 1);
    posix_spawn_file_actions_addclose(&fa, p[0]);
    posix_spawn_file_actions_addclose(&fa, p[1]);
    int rc = posix_spawn(&pid, self, &fa, NULL, plain, environ);
    close(p[1]);
    check("posix_spawn starts a program", rc == 0 && pid > 0);
    slurp(p[0], said, sizeof said);
    close(p[0]);
    check("which ran, with the descriptors it was to have", !strcmp(said, "spawned\n"));
    check("and ended with its own status",
          waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 7);
    posix_spawn_file_actions_destroy(&fa);

    /* A program that is not there: the caller is told, and has no child. */
    pid = -1;
    rc = posix_spawn(&pid, "/no/such/program", NULL, NULL, plain, environ);
    check("a program that is not there is an error, said by posix_spawn", rc == ENOENT);
    errno = 0;
    check("and leaves no child behind", waitpid(-1, &status, WNOHANG) == -1 && errno == ECHILD);

    /* In a process group of its own, which is how a shell would start a job. */
    posix_spawnattr_t attr;
    char *group[] = { "spawntest", "spawned", "group", NULL };
    check("a pipe", pipe(p) == 0);
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, p[1], 1);
    posix_spawn_file_actions_addclose(&fa, p[0]);
    posix_spawn_file_actions_addclose(&fa, p[1]);
    posix_spawnattr_init(&attr);
    posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETPGROUP);
    posix_spawnattr_setpgroup(&attr, 0);
    rc = posix_spawn(&pid, self, &fa, &attr, group, environ);
    close(p[1]);
    slurp(p[0], said, sizeof said);
    close(p[0]);
    check("a program spawned into a group of its own leads it", rc == 0 && !strcmp(said, "leads\n"));
    check("and is collected", waitpid(pid, &status, 0) == pid && WEXITSTATUS(status) == 7);
    posix_spawnattr_destroy(&attr);
    posix_spawn_file_actions_destroy(&fa);

    /* vfork, the old way to the same place. */
    check("a pipe", pipe(p) == 0);
    pid = vfork();
    if (pid == 0) {
        dup2(p[1], 1);
        close(p[0]);
        close(p[1]);
        execl(self, "spawntest", "spawned", "plain", (char *)NULL);
        _exit(127);
    }
    close(p[1]);
    slurp(p[0], said, sizeof said);
    close(p[0]);
    check("vfork and exec start one too", pid > 0 && !strcmp(said, "spawned\n"));
    check("which is collected", waitpid(pid, &status, 0) == pid && WEXITSTATUS(status) == 7);

    printf("spawntest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
