// LINK: -dynamic -fPIE -pie -Wl,-dynamic-linker,/lib/ld-musl-x86_64.so.1
/* A program whose own code makes Linux's system calls, as a program built
 * for Linux does — rustix, in every Rust program, makes them itself rather
 * than asking its C library.
 *
 * It is linked as one built for Linux is: to musl's loader by Linux's name
 * (QUARK_LINUX_INTERP), which a system running Linux's programs keeps beside
 * its own. Its loader tells it so (QUARK_AT_LINUX), and the C library it
 * runs on acts on that before main: a `syscall` instruction anywhere but in
 * the C library is not made by the kernel, whose numbers are not Linux's,
 * but handed back to the C library as SIGSYS, with every register as it was,
 * and answered there. Each check is a raw call answered as Linux would have
 * answered it, with the registers a call leaves alone left alone — through a
 * fork, in a thread, with six arguments, with SIGSYS ignored, and not after
 * an exec, which is another program.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/wait.h>
#include <unistd.h>

#include <quark/layout.h>

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

/* A system call made by this program's own code, as Linux's are made. */
static long raw(long n, long a, long b, long c, long d, long e, long f)
{
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    long ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return ret;
}

/* getpid, made raw with known values in the registers a call leaves alone:
   1 if they are still there afterwards. */
static int keeps_registers(long *pid)
{
    unsigned long rdx, r8, r9, r10;
    long ret;
    __asm__ volatile("mov $0x4444, %%rdx\n\t"
                     "mov $0x1111, %%r8\n\t"
                     "mov $0x2222, %%r9\n\t"
                     "mov $0x3333, %%r10\n\t"
                     "mov $39, %%eax\n\t"
                     "syscall\n\t"
                     "mov %%rdx, %1\n\t"
                     "mov %%r8, %2\n\t"
                     "mov %%r9, %3\n\t"
                     "mov %%r10, %4\n\t"
                     : "=a"(ret), "=r"(rdx), "=r"(r8), "=r"(r9), "=r"(r10)
                     :
                     : "rcx", "r11", "rdx", "r8", "r9", "r10", "memory");
    *pid = ret;
    return rdx == 0x4444 && r8 == 0x1111 && r9 == 0x2222 && r10 == 0x3333;
}

static void *thread_main(void *arg)
{
    (void)arg;
    return (void *)raw(186 /* gettid */, 0, 0, 0, 0, 0, 0);
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "exec") == 0) {
        /* Another program now, whose calls are made from wherever its C
           library is this time: had the last one's range come across, its
           first call would have been SIGSYS, and nothing here to answer. */
        return getpid() > 0 ? 0 : 1;
    }

    check("its loader said it was built for Linux", getauxval(QUARK_AT_LINUX) == 1);
    check("a raw getpid is answered as getpid", raw(39, 0, 0, 0, 0, 0, 0) == getpid());
    static const char line[] = "linuxtest: written by a raw write\n";
    check("a raw write writes", raw(1, 1, (long)line, sizeof line - 1, 0, 0, 0) == (long)sizeof line - 1);
    check("a raw call's failure is -errno, as Linux's is", raw(3 /* close */, -1, 0, 0, 0, 0, 0) == -EBADF);

    long pid = 0;
    check("RDX and R8 to R10 are as they were after a raw call", keeps_registers(&pid) && pid == getpid());

    long at = raw(9 /* mmap */, 0, 4096, 3 /* RW */, 0x22 /* PRIVATE|ANONYMOUS */, -1, 0);
    int mapped = at > 0 && at % 4096 == 0;
    if (mapped)
        *(volatile int *)at = 42;
    check("a raw call of six arguments is given all six",
          mapped && *(volatile int *)at == 42 && raw(11 /* munmap */, at, 4096, 0, 0, 0, 0) == 0);

    pthread_t t;
    void *tid = 0;
    int ran = pthread_create(&t, 0, thread_main, 0) == 0 && pthread_join(t, &tid) == 0;
    check("a thread's raw call is answered too", ran && (long)tid > 0 && (long)tid != gettid());

    pid_t child = fork();
    if (child == 0)
        _exit(raw(39, 0, 0, 0, 0, 0, 0) == getpid() ? 0 : 1);
    int status = -1;
    check("a fork's raw call is answered as its own",
          child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);

    signal(SIGSYS, SIG_IGN);
    check("with SIGSYS ignored, a raw call is still answered", raw(39, 0, 0, 0, 0, 0, 0) == getpid());
    signal(SIGSYS, SIG_DFL);

    /* By its path: run from a shell, argv[0] is the name it was typed as. */
    char self[256];
    if (strchr(argv[0], '/'))
        snprintf(self, sizeof self, "%s", argv[0]);
    else
        snprintf(self, sizeof self, "/usr/bin/%s", argv[0]);
    child = fork();
    if (child == 0) {
        execl(self, "linuxtest", "exec", (char *)0);
        _exit(2);
    }
    status = -1;
    int reaped = child > 0 && waitpid(child, &status, 0) == child;
    int fine = reaped && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    check("an exec'd program's calls are its own again", fine);
    if (reaped && !fine)
        printf("linuxtest: the exec'd program %s %d\n", WIFSIGNALED(status) ? "was ended by signal" : "exited with",
               WIFSIGNALED(status) ? WTERMSIG(status) : WEXITSTATUS(status));

    printf("linuxtest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
