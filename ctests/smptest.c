/* Threads that really do run at the same time.
 *
 * A machine with more than one processor runs a program's threads at once,
 * and everything a C program keeps between calls is then in two threads'
 * hands at the same moment — the C library's own state, and the state of
 * the layer under it that turns its calls into this system's. For a long
 * time there was one processor, and two threads were only ever a tick
 * apart; what is here failed the first day there were four.
 *
 * Every check is true on one processor as well, where it passes for a
 * different reason. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* Where this is staged: what a forked child execs to become a program
   that ends with a status it was told. */
#define SELF "/usr/bin/smptest"

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

#define THREADS 4

/* All of them begin together, or as near as a spin can make it. */
static atomic_int go;

static void wait_to_go(void) {
    while (!atomic_load(&go)) {
        sched_yield();
    }
}

static int in_threads(void *(*work)(void *)) {
    pthread_t t[THREADS];
    int bad = 0;
    atomic_store(&go, 0);
    for (long i = 0; i < THREADS; i++) {
        if (pthread_create(&t[i], NULL, work, (void *)i)) {
            return -1;
        }
    }
    atomic_store(&go, 1);
    for (int i = 0; i < THREADS; i++) {
        void *said = NULL;
        pthread_join(t[i], &said);
        bad += (int)(long)said;
    }
    return bad;
}

/* Memory by the megabyte, which the allocator asks the system for one
   mapping at a time: each thread's must be its own, and all of it there. */
static void *maps(void *arg) {
    long me = (long)arg;
    long bad = 0;
    wait_to_go();
    for (int round = 0; round < 200; round++) {
        size_t size = (1 << 20) + (size_t)me * 4096;
        unsigned char *p = malloc(size);
        if (!p) {
            bad++;
            continue;
        }
        memset(p, (int)(me + 1), 4096);
        p[size - 1] = (unsigned char)(me + 1);
        sched_yield();
        for (int i = 0; i < 4096; i += 511) {
            bad += p[i] != (unsigned char)(me + 1);
        }
        bad += p[size - 1] != (unsigned char)(me + 1);
        free(p);
    }
    return (void *)bad;
}

/* A file mapped, read and unmapped, by four threads at once. */
static char mapped_file[64];
static char mapped_text[] = "what every thread should find in the file\n";

static void *maps_a_file(void *arg) {
    (void)arg;
    long bad = 0;
    wait_to_go();
    for (int round = 0; round < 100; round++) {
        int fd = open(mapped_file, O_RDONLY);
        if (fd < 0) {
            bad++;
            continue;
        }
        char *p = mmap(NULL, sizeof mapped_text - 1, PROT_READ, MAP_PRIVATE, fd, 0);
        close(fd);
        if (p == MAP_FAILED) {
            bad++;
            continue;
        }
        bad += memcmp(p, mapped_text, sizeof mapped_text - 1) != 0;
        munmap(p, sizeof mapped_text - 1);
    }
    return (void *)bad;
}

/* A pipe each, marked not to wait: a read of an empty one has to say so, in
   every thread, every time. The marks are kept in one word for the whole
   program. */
static void *marks_a_pipe(void *arg) {
    (void)arg;
    long bad = 0;
    wait_to_go();
    for (int round = 0; round < 300; round++) {
        int p[2];
        if (pipe(p)) {
            bad++;
            continue;
        }
        char c;
        if (fcntl(p[0], F_SETFL, O_NONBLOCK) ||
            !(fcntl(p[0], F_GETFL) & O_NONBLOCK) ||
            read(p[0], &c, 1) != -1 || errno != EAGAIN) {
            bad++;
        }
        close(p[0]);
        close(p[1]);
    }
    return (void *)bad;
}

/* Children made and collected by four threads at once. Each is waited for
   by name, and the answer has to be that child and what it ended with:
   somebody else getting to a dead child first, with a processor to spare,
   once answered that process 0 had ended. Every other child becomes
   another program before it ends, which is what a thread that runs a
   command does — and a program going, while its siblings' children come
   and go on the other processors. */
static void *forks(void *arg) {
    long me = (long)arg;
    long bad = 0;
    wait_to_go();
    for (int round = 0; round < 60; round++) {
        int code = (int)((me * 60 + round) % 100) + 1;
        /* Written before the fork: the child of a program with threads has
           one thread and whatever locks the others held, and does nothing
           that could want one. */
        char said[8];
        snprintf(said, sizeof said, "%d", code);
        pid_t pid = fork();
        if (pid == 0) {
            if (round & 1) {
                execl(SELF, SELF, "--exit", said, (char *)NULL);
                _exit(0);
            }
            _exit(code);
        }
        if (pid < 0) {
            bad++;
            continue;
        }
        int st = 0;
        pid_t got = waitpid(pid, &st, 0);
        if (got != pid || !WIFEXITED(st) || WEXITSTATUS(st) != code) {
            bad++;
        }
    }
    return (void *)bad;
}

static void *nothing(void *arg) {
    return arg;
}

static atomic_int stay;

static void *stays(void *arg) {
    while (atomic_load(&stay)) {
        usleep(1000);
    }
    return arg;
}

/* A handler, and four threads on their way out of system calls while the
   signal arrives: it runs once for each time it was raised and not once for
   each thread that noticed. */
static atomic_int handled;

static void on_usr1(int sig) {
    (void)sig;
    atomic_fetch_add(&handled, 1);
}

static atomic_int calling;

static long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void *makes_calls(void *arg) {
    (void)arg;
    wait_to_go();
    while (atomic_load(&calling)) {
        getppid();
    }
    return NULL;
}

int main(int argc, char **argv) {
    /* What a child becomes: a program that ends as it was told to. */
    if (argc > 2 && !strcmp(argv[1], "--exit")) {
        _exit(atoi(argv[2]));
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("threads at the same time:\n");

    long processors = sysconf(_SC_NPROCESSORS_ONLN);
    int on = sched_getcpu();
    printf("        %ld processor(s); this thread is on %d\n", processors, on);
    check("the machine says how many processors it has", processors >= 1 && processors <= 1024);
    check("and which one a thread is on", on >= 0 && on < processors);
    cpu_set_t set;
    CPU_ZERO(&set);
    check("every one of them is this program's to run on",
          sched_getaffinity(0, sizeof set, &set) == 0 && CPU_COUNT(&set) == processors);

    /* First, before any thread of this program has ended. The C library
       blocks every signal in a thread that is ending, and the mask is the
       program's, not the thread's, until the kernel keeps one for each: so
       once a thread has ended, a handler is not run again. What is asked
       here is what more than one processor changed — that four threads
       which all see a signal waiting run its handler once between them. */
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_usr1;
    sa.sa_flags = SA_RESTART;
    sigaction(SIGUSR1, &sa, NULL);
    pthread_t t[THREADS];
    atomic_store(&calling, 1);
    atomic_store(&go, 0);
    int started = 1;
    for (long i = 0; i < THREADS; i++) {
        started &= pthread_create(&t[i], NULL, makes_calls, NULL) == 0;
    }
    atomic_store(&go, 1);
    int raised = 0;
    for (int i = 0; i < 50 && started; i++) {
        int before = atomic_load(&handled);
        if (kill(getpid(), SIGUSR1) == 0) {
            raised++;
        }
        /* One at a time: a second raised before the first has been taken is
           the same signal, here as anywhere. */
        long until = now_ms() + 2000;
        while (atomic_load(&handled) == before && now_ms() < until) {
            usleep(1000);
        }
        if (atomic_load(&handled) == before) {
            break;
        }
    }
    atomic_store(&calling, 0);
    for (int i = 0; i < THREADS; i++) {
        pthread_join(t[i], NULL);
    }
    check("a handler runs once for each signal, with four threads making calls",
          started && raised == 50 && atomic_load(&handled) == 50);

    check("four threads each get the memory they ask for", in_threads(maps) == 0);

    snprintf(mapped_file, sizeof mapped_file, "/tmp/smptest.%d", (int)getpid());
    int fd = open(mapped_file, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    int wrote = fd >= 0 && write(fd, mapped_text, sizeof mapped_text - 1) == (ssize_t)(sizeof mapped_text - 1);
    if (fd >= 0) {
        close(fd);
    }
    check("and each maps a file and finds the file in it", wrote && in_threads(maps_a_file) == 0);
    unlink(mapped_file);

    check("a descriptor marked not to wait does not, whichever thread marked it",
          in_threads(marks_a_pipe) == 0);

    check("four threads each collect the children they made, and the programs those became",
          in_threads(forks) == 0);

    /* A thread that has ended and been joined is gone, and gives back its
       place: there are fewer places for tasks in the whole system than this
       makes threads. And a thread is not a child. It is joined, not waited
       for, and a program with threads and no children has nothing to wait
       for — which a shell asks, to know whether to go on waiting. */
    int made = 0;
    for (int i = 0; i < 150; i++) {
        pthread_t once;
        if (pthread_create(&once, NULL, nothing, NULL)) {
            break;
        }
        pthread_join(once, NULL);
        made++;
    }
    check("a hundred and fifty threads, one after another", made == 150);
    int st = 0;
    pthread_t staying;
    atomic_store(&stay, 1);
    int there = pthread_create(&staying, NULL, stays, NULL) == 0;
    errno = 0;
    pid_t nobody = waitpid(-1, &st, WNOHANG);
    int none = nobody == -1 && errno == ECHILD;
    atomic_store(&stay, 0);
    if (there) {
        pthread_join(staying, NULL);
    }
    check("and a thread is nobody's child: there is nothing to wait for", there && none);

    if (failed) {
        printf("smptest: FAILED\n");
        return 1;
    }
    printf("smptest: ok\n");
    return 0;
}
