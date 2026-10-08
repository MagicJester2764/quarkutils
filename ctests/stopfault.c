/* A program stopped while it takes a page fault is served at its own
 * fault's address.
 *
 * A fault in ring 3 waits at the kernel's door for the lock, and its task
 * can be stopped there by whoever held it: the program was sent SIGSTOP.
 * Stopped, it gives up its processor, other programs run there and fault,
 * and it may be continued on another processor. Where the fault was is the
 * processor's CR2, and it was read after the stop: a child of jobtest,
 * stopped straight after its fork as it took its first copy-on-write fault,
 * was served at an address some other program had faulted on, and ended
 * for touching that ([UPFAULT ... err=0x7], a write to a page that was
 * there, and at the address it named nothing at all).
 *
 * Children each map, touch and unmap a region of their own, so that they
 * fault all the time and at addresses none of the others has; their parent
 * stops and continues each of them thousands of times, the first time
 * straight after the fork. A fault served at another child's address ends
 * the child with SIGSEGV. On one processor nothing faults while a stopped
 * task waits at the door, and this passes either way.
 *
 * Exits 0 only if every child was still there, stopped and continued, at
 * the end.
 */
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CHILDREN 3
#define ROUNDS 2000
#define SECONDS 20
#define PAGES 64
#define PAGE 4096UL

/* Map a region at `mine`, write to each of its pages — each write a fault
   — and give it back, for ever. */
static void fault_for_ever(char *mine) {
    for (unsigned n = 1;; n++) {
        char *p = mmap(mine, PAGES * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
        if (p != mine) {
            _exit(3);
        }
        for (unsigned long i = 0; i < PAGES; i++) {
            p[i * PAGE] = (char)n;
        }
        munmap(p, PAGES * PAGE);
    }
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("stopfault:\n");
    /* Room for every child's region, given back: each child maps its own
       part of it, and nothing else is there. */
    char *room = mmap(NULL, CHILDREN * PAGES * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (room == MAP_FAILED) {
        printf("stopfault: no room: FAILED\n");
        return 1;
    }
    munmap(room, CHILDREN * PAGES * PAGE);
    pid_t kids[CHILDREN];
    for (int i = 0; i < CHILDREN; i++) {
        kids[i] = fork();
        if (kids[i] == 0) {
            fault_for_ever(room + i * PAGES * PAGE);
        }
        /* Straight after the fork, as jobtest does. */
        kill(kids[i], SIGSTOP);
    }
    int failed = 0;
    long stops = 0;
    double until = now() + SECONDS;
    for (int r = 0; r < ROUNDS && !failed && now() < until; r++) {
        for (int i = 0; i < CHILDREN && !failed; i++) {
            int status = 0;
            if (r > 0) {
                kill(kids[i], SIGSTOP);
            }
            if (waitpid(kids[i], &status, WUNTRACED) != kids[i] || !WIFSTOPPED(status)) {
                if (WIFSIGNALED(status)) {
                    printf("  child %d was ended by signal %d, after %ld stops\n", i, WTERMSIG(status), stops);
                } else {
                    printf("  child %d ended with status 0x%x, after %ld stops\n", i, status, stops);
                }
                failed = 1;
                break;
            }
            stops++;
            kill(kids[i], SIGCONT);
        }
    }
    for (int i = 0; i < CHILDREN; i++) {
        kill(kids[i], SIGKILL);
        waitpid(kids[i], NULL, 0);
    }
    printf("  %ld stops and continues\n", stops);
    printf("  %s  a child stopped while it faults is served at its own fault's address\n", failed ? "FAIL" : "ok  ");
    printf("stopfault: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
