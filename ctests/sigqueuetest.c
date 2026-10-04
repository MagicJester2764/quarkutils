/* Signals that queue, and say who.
 *
 * A real-time signal raised while one of its number is waiting waits behind
 * it, with what it carried, and each is handled in turn; a signal below 32
 * raised again while it waits is the same one. A handler, and sigwaitinfo,
 * are told who raised a signal and as what — and for SIGCHLD which child it
 * was and what became of it, and for a fault how it was one. The layer
 * answered sigqueue with ENOSYS, every siginfo said only a process id, and
 * three of one real-time signal were one.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

/* What the handler was told, the last eight times. */
static volatile int ran;
static volatile int values[8];
static volatile int codes[8];
static volatile pid_t pids[8];
static volatile uid_t uids[8];

static void on_signal(int sig, siginfo_t *si, void *uc)
{
    (void)sig;
    (void)uc;
    if (ran < 8) {
        values[ran] = si->si_value.sival_int;
        codes[ran] = si->si_code;
        pids[ran] = si->si_pid;
        uids[ran] = si->si_uid;
    }
    ran++;
}

static sigjmp_buf back;
static volatile int fault_code;
static void *volatile fault_addr;

static void on_fault(int sig, siginfo_t *si, void *uc)
{
    (void)sig;
    (void)uc;
    fault_code = si->si_code;
    fault_addr = si->si_addr;
    siglongjmp(back, 1);
}

static void handle(int sig, void (*how)(int, siginfo_t *, void *))
{
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = how;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
}

static sigset_t only(int sig)
{
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, sig);
    return set;
}

int main(void)
{
    printf("sigqueuetest:\n");
    int rt = SIGRTMIN + 2;
    sigset_t set, old;

    /* Three, held back, then let through: three runs, in order. */
    handle(rt, on_signal);
    set = only(rt);
    sigprocmask(SIG_BLOCK, &set, &old);
    int queued = 1;
    for (int i = 1; i <= 3; i++)
        queued &= sigqueue(getpid(), rt, (union sigval){.sival_int = 100 + i}) == 0;
    check("a real-time signal is queued three times while held back", queued);
    sigprocmask(SIG_SETMASK, &old, NULL);
    check("and its handler runs three times when it is let through", ran == 3);
    check("with each value, in the order they were queued",
          ran == 3 && values[0] == 101 && values[1] == 102 && values[2] == 103);
    check("told each was queued, by this process and its user",
          ran >= 1 && codes[0] == SI_QUEUE && pids[0] == getpid() && uids[0] == getuid());

    /* One below 32, raised twice while held back, is one. */
    ran = 0;
    handle(SIGUSR1, on_signal);
    set = only(SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, &old);
    kill(getpid(), SIGUSR1);
    kill(getpid(), SIGUSR1);
    sigprocmask(SIG_SETMASK, &old, NULL);
    check("a signal below 32 raised twice while held back runs once", ran == 1);
    check("told it was a kill, by whom", ran == 1 && codes[0] == SI_USER && pids[0] == getpid() && uids[0] == getuid());

    /* sigwaitinfo takes them, one at a time, with everything. */
    set = only(rt);
    sigprocmask(SIG_BLOCK, &set, &old);
    sigqueue(getpid(), rt, (union sigval){.sival_int = 7});
    sigqueue(getpid(), rt, (union sigval){.sival_int = 8});
    siginfo_t si;
    int a = sigwaitinfo(&set, &si);
    int a_value = si.si_value.sival_int, a_code = si.si_code;
    int b = sigwaitinfo(&set, &si);
    int b_value = si.si_value.sival_int;
    check("sigwaitinfo takes them one at a time, each with its value",
          a == rt && b == rt && a_value == 7 && b_value == 8 && a_code == SI_QUEUE);
    struct timespec none = {0, 0};
    check("and then there are none", sigtimedwait(&set, &si, &none) == -1 && errno == EAGAIN);

    /* There is room for so many, and no more. */
    int n = 0;
    while (n < 1000 && sigqueue(getpid(), rt, (union sigval){.sival_int = n}) == 0)
        n++;
    int why = errno;
    check("a queue with no room refuses with EAGAIN", n >= 32 && n < 1000 && why == EAGAIN);
    struct rlimit room;
    check("getrlimit says how many may wait, and no more than did",
          getrlimit(RLIMIT_SIGPENDING, &room) == 0 && room.rlim_cur != RLIM_INFINITY && room.rlim_cur <= (rlim_t)n);
    int taken = 0;
    while (sigtimedwait(&set, &si, &none) == rt)
        taken++;
    check("every one that was queued is taken", taken == n);
    sigprocmask(SIG_SETMASK, &old, NULL);

    /* SIGCHLD says which child, and what became of it. */
    set = only(SIGCHLD);
    sigprocmask(SIG_BLOCK, &set, &old);
    int status;
    pid_t c = fork();
    if (c == 0)
        _exit(7);
    waitpid(c, &status, 0);
    int w = sigwaitinfo(&set, &si);
    check("SIGCHLD says a child exited, which, and with what",
          w == SIGCHLD && si.si_code == CLD_EXITED && si.si_pid == c && si.si_status == 7);
    c = fork();
    if (c == 0) {
        for (;;)
            pause();
    }
    kill(c, SIGTERM);
    waitpid(c, &status, 0);
    w = sigwaitinfo(&set, &si);
    check("or that a signal ended it, and which",
          w == SIGCHLD && si.si_code == CLD_KILLED && si.si_pid == c && si.si_status == SIGTERM);
    sigprocmask(SIG_SETMASK, &old, NULL);

    /* A fault says how it was one. */
    handle(SIGSEGV, on_fault);
    static volatile unsigned long nowhere = 16;
    if (!sigsetjmp(back, 1))
        *(volatile int *)nowhere = 1;
    check("touching what is not there is SEGV_MAPERR, at the address", fault_code == SEGV_MAPERR && fault_addr == (void *)16);
    fault_code = 0;
    if (!sigsetjmp(back, 1))
        *(volatile char *)(void *)check = 0;
    check("writing to the program's code is SEGV_ACCERR", fault_code == SEGV_ACCERR && fault_addr == (void *)check);

    printf("sigqueuetest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
