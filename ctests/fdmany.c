/* A program's descriptors at a desktop's size: a thousand of them, a limit
 * it can raise, and a child made by fork that keeps its parent's limit. The
 * copies are of a pipe of its own: what it was started with is its starter's
 * to say (runtests gives a test only 1 and 2). */
#include <errno.h>
#include <stdio.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    struct rlimit rl;
    if (getrlimit(RLIMIT_NOFILE, &rl) != 0 || rl.rlim_cur != 1024 || rl.rlim_max != 65536) {
        printf("fdmany: FAILED (%lu/%lu)\n", (unsigned long)rl.rlim_cur, (unsigned long)rl.rlim_max);
        return 1;
    }
    int p[2];
    if (pipe(p) != 0) {
        printf("fdmany: FAILED (pipe)\n");
        return 1;
    }
    int n = 0;
    while (n < 1000 && dup(p[0]) >= 0) {
        n++;
    }
    if (n != 1000) {
        printf("fdmany: FAILED (%d dups)\n", n);
        return 1;
    }
    rl.rlim_cur = 4096;
    if (setrlimit(RLIMIT_NOFILE, &rl) != 0) {
        printf("fdmany: FAILED (setrlimit)\n");
        return 1;
    }
    if (dup2(p[0], 3000) != 3000) {
        printf("fdmany: FAILED (dup2 to 3000: errno %d)\n", errno);
        return 1;
    }
    errno = 0;
    if (dup2(p[0], 70000) != -1 || errno != EBADF) {
        printf("fdmany: FAILED (dup2 to 70000: errno %d)\n", errno);
        return 1;
    }
    pid_t c = fork();
    if (c == 0) {
        struct rlimit mine;
        _exit(getrlimit(RLIMIT_NOFILE, &mine) == 0 && mine.rlim_cur == 4096 ? 0 : 1);
    }
    int st = 0;
    if (c < 0 || waitpid(c, &st, 0) != c || !WIFEXITED(st) || WEXITSTATUS(st) != 0) {
        printf("fdmany: FAILED (a forked child's limit)\n");
        return 1;
    }
    printf("fdmany: ok\n");
    return 0;
}
