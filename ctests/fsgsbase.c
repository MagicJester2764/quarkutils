/* A program's own FS and GS bases, read and written in the program
 * (FSGSBASE: rdfsbase, wrfsbase, rdgsbase, wrgsbase).
 *
 * They were the kernel's alone: CR4.FSGSBASE was off, each of the four was
 * an invalid instruction, and a program that used one — a runtime that keeps
 * its own per-thread state in GS, as some do — was ended with SIGILL. Turned
 * on, each task's two bases are its own: kept across a switch, copied by a
 * fork, cleared by an exec, and the program is told it may (AT_HWCAP2 bit 1,
 * HWCAP2_FSGSBASE).
 *
 * A child sets its GS base, yields and sleeps, and reads back what it set; a
 * thread of it reads its own, nought; a child it forks reads the one it was
 * forked with; one it starts as another program reads nought; and its FS
 * base is its thread pointer, as the C library set it. On a processor
 * without them it says so and passes.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <cpuid.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed = 1;
    }
}

static unsigned long rdgsbase(void) {
    unsigned long v;
    __asm__ volatile("rdgsbase %0" : "=r"(v));
    return v;
}

static void wrgsbase(unsigned long v) {
    __asm__ volatile("wrgsbase %0" : : "r"(v) : "memory");
}

static unsigned long rdfsbase(void) {
    unsigned long v;
    __asm__ volatile("rdfsbase %0" : "=r"(v));
    return v;
}

#define MINE 0x0000123456789000UL

static void *reads_its_own(void *arg) {
    *(unsigned long *)arg = rdgsbase();
    return NULL;
}

/* What a child finds, a bit for each thing that is not as it should be. */
static int child(const char *self) {
    int wrong = 0;
    wrgsbase(MINE);
    sched_yield();
    struct timespec t = {0, 10000000L};
    nanosleep(&t, NULL);
    if (rdgsbase() != MINE) {
        wrong |= 1;
    }
    unsigned long theirs = 1;
    pthread_t th;
    if (pthread_create(&th, NULL, reads_its_own, &theirs) != 0 || pthread_join(th, NULL) != 0 || theirs != 0) {
        wrong |= 2;
    }
    if (rdgsbase() != MINE) {
        wrong |= 1;
    }
    pid_t g = fork();
    if (g == 0) {
        _exit(rdgsbase() == MINE ? 0 : 1);
    }
    int status = 0;
    if (g < 0 || waitpid(g, &status, 0) != g || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        wrong |= 4;
    }
    g = fork();
    if (g == 0) {
        execlp(self, self, "fresh", (char *)NULL);
        _exit(9);
    }
    if (g < 0 || waitpid(g, &status, 0) != g || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        wrong |= 8;
    }
    if (rdfsbase() != (unsigned long)pthread_self()) {
        wrong |= 16;
    }
    return wrong;
}

int main(int argc, char **argv) {
    /* Started as another program by a child that had a GS base of its own:
       nought now, and the thread pointer in FS. */
    if (argc > 1 && strcmp(argv[1], "fresh") == 0) {
        return rdgsbase() == 0 && rdfsbase() == (unsigned long)pthread_self() ? 0 : 1;
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("fsgsbase:\n");
    unsigned a, b, c, d;
    if (!__get_cpuid_count(7, 0, &a, &b, &c, &d) || !(b & 1)) {
        printf("fsgsbase: this processor has none: skipped\n");
        return 0;
    }
    check("a program is told it may use them (AT_HWCAP2 bit 1)", (getauxval(AT_HWCAP2) & 2) != 0);
    pid_t k = fork();
    if (k == 0) {
        _exit(child(argv[0]));
    }
    int status = 0;
    int ok = k > 0 && waitpid(k, &status, 0) == k;
    int code = ok && WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ok && WIFSIGNALED(status)) {
        printf("  (the child was ended by signal %d)\n", WTERMSIG(status));
    }
    check("a child sets its GS base, yields and sleeps, and reads it back", code >= 0 && !(code & 1));
    check("a thread of it reads its own, nought", code >= 0 && !(code & 2));
    check("a child it forks reads the one it had", code >= 0 && !(code & 4));
    check("a program it starts reads nought, and its thread pointer in FS", code >= 0 && !(code & 8));
    check("its FS base is its thread pointer", code >= 0 && !(code & 16));
    printf("fsgsbase: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
