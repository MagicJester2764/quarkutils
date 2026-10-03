/* Where a C program's things are is chosen at random each time it is run:
 * its stack (by execve, which builds it), what malloc gives, what mmap gives
 * with no address asked for. A program that knew where a thing was in one
 * run knows nothing about the next. Run twice from here, with an argument
 * that has it say where its things are and stop. A forked child is the same
 * program, and has its parent's places.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* Where this run put a local, a small and a large allocation, and a page. */
static void where(unsigned long out[4]) {
    volatile int local = 0;
    void *small = malloc(16);
    void *large = malloc(1 << 20);
    void *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    out[0] = (unsigned long)&local;
    out[1] = (unsigned long)small;
    out[2] = (unsigned long)large;
    out[3] = (unsigned long)page;
}

/* Run this program again with `--where`, and read what it says. */
static int run_again(const char *self, unsigned long out[4]) {
    int p[2];
    if (pipe(p) != 0) {
        return 0;
    }
    pid_t pid = fork();
    if (pid == 0) {
        dup2(p[1], 1);
        close(p[0]);
        close(p[1]);
        execlp(self, self, "--where", (char *)0);
        _exit(127);
    }
    close(p[1]);
    char text[256];
    size_t got = 0;
    ssize_t n;
    while (got < sizeof text - 1 && (n = read(p[0], text + got, sizeof text - 1 - got)) > 0) {
        got += (size_t)n;
    }
    close(p[0]);
    text[got] = 0;
    int st;
    waitpid(pid, &st, 0);
    return sscanf(text, "%lx %lx %lx %lx", &out[0], &out[1], &out[2], &out[3]) == 4;
}

int main(int argc, char **argv) {
    unsigned long here[4];
    if (argc > 1 && strcmp(argv[1], "--where") == 0) {
        where(here);
        printf("%lx %lx %lx %lx\n", here[0], here[1], here[2], here[3]);
        return 0;
    }
    printf("layouttest: where a program's things are\n");
    unsigned long one[4], two[4];
    int ran = run_again(argv[0], one) && run_again(argv[0], two);
    check("a program run twice says where its things are", ran);
    check("its stack is somewhere else each time", ran && one[0] != two[0]);
    check("and what malloc gives", ran && one[1] != two[1] && one[2] != two[2]);
    check("and what mmap gives", ran && one[3] != two[3]);

    where(here);
    int p[2];
    unsigned long theirs[4] = {0, 0, 0, 0};
    if (pipe(p) == 0) {
        pid_t pid = fork();
        if (pid == 0) {
            unsigned long mine[4];
            where(mine);
            write(p[1], mine, sizeof mine);
            _exit(0);
        }
        read(p[0], theirs, sizeof theirs);
        waitpid(pid, NULL, 0);
    }
    /* The child's next allocation is where the parent's would have been: it
       is a copy of the parent, arena and all, until it does something else. */
    unsigned long next[4];
    where(next);
    check("a forked child is the same program, its places with it",
          theirs[0] != 0 && theirs[3] == next[3]);

    printf("layouttest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
