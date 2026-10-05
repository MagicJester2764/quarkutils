/* A thread's own store, read straight back, while another thread of the
 * program forks as fast as it can: every fork makes each page the program
 * could write copy-on-write again, and a thread's next store to one is a
 * fault that gives the program a copy of its own — or the page back, once
 * the child has gone.
 *
 * cargo, building with four processors, now and then read back a byte of
 * malloc's that it had stored seven instructions before and found what had
 * been there before the store.
 *
 *   cowstress [SECONDS]
 *
 * Exits 0 only if every thread always read what it had stored, and every
 * word held what its thread last put there.
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

#define WORKERS 3
#define PAGES 64
#define WORDS (PAGES * 4096 / 8)

static volatile int failed;
static volatile int stop;

/* One region every thread writes, a word in WORKERS each: every page has
   all of them storing to it at once, as a heap's pages do. */
static volatile uint64_t *words;
static uint64_t *expect;

struct worker {
    int id;
    unsigned long stores;
};

static void *work(void *arg)
{
    struct worker *w = arg;
    uint64_t s = 0x9E3779B97F4A7C15ULL * (uint64_t)(w->id + 1);
    uint64_t n = 0;
    while (!stop && !failed) {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        size_t i = (s % (WORDS / WORKERS)) * WORKERS + (size_t)w->id;
        uint64_t v = (uint64_t)w->id << 56 | ++n;
        words[i] = v;
        uint64_t back = words[i];
        if (back != v) {
            fprintf(stderr, "cowstress: thread %d stored %#llx at %p and read back %#llx\n",
                    w->id, (unsigned long long)v, (void *)&words[i], (unsigned long long)back);
            failed = 1;
            break;
        }
        expect[i] = v;
        if ((n & 8191) == 0) {
            for (size_t j = (size_t)w->id; j < WORDS; j += WORKERS) {
                if (words[j] != expect[j]) {
                    fprintf(stderr, "cowstress: thread %d's word at %p holds %#llx, not the %#llx it stored\n",
                            w->id, (void *)&words[j], (unsigned long long)words[j],
                            (unsigned long long)expect[j]);
                    failed = 2;
                    break;
                }
            }
        }
    }
    w->stores = n;
    return 0;
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "child") == 0)
        return 7;
    long seconds = argc > 1 ? atol(argv[1]) : 20;
    char *self = argv[0][0] == '/' ? argv[0] : "/usr/bin/cowstress";

    static struct worker w[WORKERS];
    pthread_t t[WORKERS];
    words = mmap(0, PAGES * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    expect = mmap(0, PAGES * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (words == MAP_FAILED || expect == MAP_FAILED) {
        perror("cowstress: mmap");
        return 2;
    }
    for (int i = 0; i < WORKERS; i++) {
        w[i].id = i;
        pthread_create(&t[i], 0, work, &w[i]);
    }

    /* The ways a program is started from one with threads running: a child
       that ends at once, one that writes first, one that becomes another
       program, and posix_spawn. */
    unsigned long forks = 0;
    time_t until = time(0) + seconds;
    while (!failed && time(0) < until) {
        pid_t pid;
        int status = 0;
        switch (forks++ % 4) {
        case 0:
            pid = fork();
            if (pid == 0)
                _exit(7);
            break;
        case 1:
            pid = fork();
            if (pid == 0) {
                words[(forks * 61) % WORDS] = 0;
                _exit(7);
            }
            break;
        case 2:
            pid = fork();
            if (pid == 0) {
                char *args[] = {self, "child", 0};
                execve(self, args, environ);
                _exit(127);
            }
            break;
        default: {
            char *args[] = {self, "child", 0};
            if (posix_spawn(&pid, self, 0, 0, args, environ) != 0)
                pid = -1;
        }
        }
        if (pid < 0 || waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 7) {
            fprintf(stderr, "cowstress: a child did not end as it does\n");
            failed = 3;
        }
    }
    stop = 1;
    unsigned long stores = 0;
    for (int i = 0; i < WORKERS; i++) {
        pthread_join(t[i], 0);
        stores += w[i].stores;
    }
    printf("  %s  %d threads store and read back while %lu children start, for %ld s (%lu stores)\n",
           failed ? "FAIL" : "ok  ", WORKERS, forks, seconds, stores);
    printf("cowstress: %s\n", failed ? "FAILED" : "passed");
    return failed != 0;
}
