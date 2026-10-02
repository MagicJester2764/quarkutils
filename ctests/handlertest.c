/* What a handler the kernel runs gives a C program, that a handler run at a
 * system call did not: it interrupts a program that is computing; it keeps
 * the floating-point registers of whatever it interrupted; it is run by a
 * thread that does not hold its signal back, or by the thread it was sent
 * to; it is handed where the program was, and can change it; it can run on
 * a stack of its own, which is how a program survives running out of its
 * own; and every wait it ends says so as Linux says it — a pipe's read and a
 * wait for a child made again or not as SA_RESTART says, a poll under
 * another mask ended by a signal that was already waiting. A signal held
 * back waits whatever it would do, and can be taken without a handler.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fenv.h>
#include <poll.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static volatile sig_atomic_t seen;
static volatile sig_atomic_t stop;

static long elapsed_ms(const struct timespec *since) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - since->tv_sec) * 1000 + (now.tv_nsec - since->tv_nsec) / 1000000;
}

static void handle(int sig, void (*fn)(int), int flags) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = fn;
    sa.sa_flags = flags;
    sigaction(sig, &sa, NULL);
}

/* SIGALRM in `ms` milliseconds, once. */
static void alarm_ms(long ms) {
    struct itimerval it;
    memset(&it, 0, sizeof it);
    it.it_value.tv_sec = ms / 1000;
    it.it_value.tv_usec = (ms % 1000) * 1000;
    setitimer(ITIMER_REAL, &it, NULL);
}

/* Run `fn` in a child and say how it ended: its status, minus the signal
   that ended it, or -1000 if it was still going after `ms`. */
static int in_child(int (*fn)(void), long ms) {
    pid_t pid = fork();
    if (pid == 0) {
        _exit(fn());
    }
    struct timespec t0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    for (;;) {
        int st;
        if (waitpid(pid, &st, WNOHANG) == pid) {
            return WIFEXITED(st) ? WEXITSTATUS(st) : -WTERMSIG(st);
        }
        if (elapsed_ms(&t0) > ms) {
            kill(pid, SIGKILL);
            waitpid(pid, &st, 0);
            return -1000;
        }
        usleep(5000);
    }
}

static void stopping(int sig) {
    (void)sig;
    stop = 1;
    seen++;
}

/* A loop with no system call in it, until a handler says stop. */
static int computes(void) {
    handle(SIGALRM, stopping, 0);
    stop = 0;
    alarm_ms(50);
    unsigned long n = 0;
    while (!stop && n < 40000000000UL) {
        n++;
    }
    return stop ? 7 : 8;
}

/* Floating point, in registers, across handlers that use it too. */
static volatile double sink;
static volatile int handler_rounding;

static void sums(int sig) {
    (void)sig;
    double x = 1.0;
    for (int i = 0; i < 2000; i++) {
        x = x * 1.0000003 + 0.25;
    }
    sink = x;
    handler_rounding = fegetround();
    seen++;
}

static double work(long n) {
    double a = 0.0, b = 1.0;
    for (long i = 0; i < n; i++) {
        a += b * 0.5;
        b = b * 1.00000001;
    }
    return a + b;
}

static void every_ms(long ms) {
    struct itimerval it;
    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = ms * 1000;
    it.it_interval.tv_usec = ms * 1000;
    setitimer(ITIMER_REAL, &it, NULL);
}

/* Which thread ran the handler. */
static volatile pthread_t ran_in;
static volatile int worker_ready, worker_done;

static void whose(int sig) {
    (void)sig;
    ran_in = pthread_self();
    seen++;
}

static void *worker(void *arg) {
    (void)arg;
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, SIGUSR1);
    pthread_sigmask(SIG_UNBLOCK, &s, NULL);
    worker_ready = 1;
    while (!worker_done) {
        usleep(1000);
    }
    return NULL;
}

/* Where a fault was, and the instruction after it. */
static void *faulted_at;
static int fault_code;

static void step_over(int sig, siginfo_t *si, void *ctx) {
    (void)sig;
    ucontext_t *uc = ctx;
    faulted_at = si->si_addr;
    fault_code = si->si_code;
    uc->uc_mcontext.gregs[REG_RIP] += 3;
    seen++;
}

/* One instruction three bytes long, that writes where it is told. */
void poke3(void *p);
__asm__(".text\n.globl poke3\npoke3:\n    movb $1, (%rdi)\n    ret\n");

/* A stack of its own. */
static char *alt;
#define ALT_SIZE (64 * 1024)
static volatile char *where;
static volatile int reported_on;

static void on_alt(int sig) {
    (void)sig;
    char here;
    stack_t cur;
    where = &here;
    reported_on = sigaltstack(NULL, &cur) == 0 && (cur.ss_flags & SS_ONSTACK);
    seen++;
}

static sigjmp_buf out;

static void leap(int sig) {
    (void)sig;
    siglongjmp(out, 1);
}

static long deep(long n) {
    volatile char pad[4096];
    pad[0] = (char)n;
    return n ? deep(n - 1) + pad[0] : 0;
}

/* Recursion past the end of the stack, saved by a handler on another. */
static int overflows(void) {
    stack_t ss;
    ss.ss_sp = mmap(NULL, ALT_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    ss.ss_size = ALT_SIZE;
    ss.ss_flags = 0;
    if (ss.ss_sp == MAP_FAILED || sigaltstack(&ss, NULL) != 0) {
        return 2;
    }
    handle(SIGSEGV, leap, SA_ONSTACK);
    if (sigsetjmp(out, 1)) {
        return 7;
    }
    deep(1L << 30);
    return 3;
}

static void note(int sig) {
    (void)sig;
    seen++;
}

/* A program that holds SIGTERM back is not ended by it until it lets it
   through: it says it is still there, down this pipe, first. */
static int still_here[2];

static int holds_term(void) {
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, SIGTERM);
    sigprocmask(SIG_BLOCK, &s, NULL);
    kill(getpid(), SIGTERM);
    usleep(20000);
    write(still_here[1], "h", 1);
    sigprocmask(SIG_UNBLOCK, &s, NULL);
    return 9;
}

int main(void) {
    struct timespec t0;

    check("a handler interrupts a program that makes no system call", in_child(computes, 20000) == 7);

    /* Floating point. */
    fesetround(FE_UPWARD);
    double alone = work(30000000);
    handle(SIGALRM, sums, SA_RESTART);
    seen = 0;
    every_ms(1);
    double interrupted = work(30000000);
    every_ms(0);
    check("a handler that computes leaves the floating-point registers it interrupted as they were",
          seen > 3 && alone == interrupted);
    check("and the rounding they were in", fegetround() == FE_UPWARD);
    check("having run in the usual one itself", handler_rounding == FE_TONEAREST);
    fesetround(FE_TONEAREST);
    handle(SIGALRM, SIG_DFL, 0);

    /* Threads: whose mask, and whose handler. */
    handle(SIGUSR1, whose, 0);
    handle(SIGUSR2, whose, 0);
    sigset_t s, cur;
    sigemptyset(&s);
    sigaddset(&s, SIGUSR1);
    pthread_sigmask(SIG_BLOCK, &s, NULL);
    pthread_t w;
    worker_ready = worker_done = 0;
    if (pthread_create(&w, NULL, worker, NULL) == 0) {
        while (!worker_ready) {
            usleep(1000);
        }
        pthread_sigmask(SIG_BLOCK, NULL, &cur);
        check("a thread's mask is its own", sigismember(&cur, SIGUSR1));
        seen = 0;
        ran_in = 0;
        kill(getpid(), SIGUSR1);
        clock_gettime(CLOCK_MONOTONIC, &t0);
        while (!seen && elapsed_ms(&t0) < 2000) {
            usleep(1000);
        }
        check("a signal is run by a thread that does not hold it back", seen == 1 && pthread_equal(ran_in, w));
        seen = 0;
        ran_in = 0;
        pthread_kill(w, SIGUSR2);
        clock_gettime(CLOCK_MONOTONIC, &t0);
        while (!seen && elapsed_ms(&t0) < 2000) {
            usleep(1000);
        }
        check("pthread_kill runs it in the thread named", seen == 1 && pthread_equal(ran_in, w));
        seen = 0;
        ran_in = 0;
        pthread_kill(pthread_self(), SIGUSR2);
        check("and in the caller, before it returns", seen == 1 && pthread_equal(ran_in, pthread_self()));
        worker_done = 1;
        pthread_join(w, NULL);
    } else {
        check("a thread", 0);
    }
    pthread_sigmask(SIG_UNBLOCK, &s, NULL);
    seen = 0;

    /* Taken, not run. */
    handle(SIGUSR2, SIG_DFL, 0);
    sigemptyset(&s);
    sigaddset(&s, SIGUSR2);
    sigprocmask(SIG_BLOCK, &s, NULL);
    kill(getpid(), SIGUSR2);
    sigset_t pend;
    check("a signal held back that would end the program waits", sigpending(&pend) == 0 && sigismember(&pend, SIGUSR2));
    siginfo_t info;
    memset(&info, 0, sizeof info);
    check("and is taken by sigwaitinfo, which says who sent it",
          sigwaitinfo(&s, &info) == SIGUSR2 && info.si_pid == getpid() && info.si_code == SI_USER);
    struct timespec short_wait = {0, 50000000};
    clock_gettime(CLOCK_MONOTONIC, &t0);
    check("sigtimedwait gives up when its time does",
          sigtimedwait(&s, &info, &short_wait) == -1 && errno == EAGAIN && elapsed_ms(&t0) >= 45);
    pid_t sender = fork();
    if (sender == 0) {
        usleep(100000);
        kill(getppid(), SIGUSR2);
        _exit(0);
    }
    struct timespec long_wait = {5, 0};
    memset(&info, 0, sizeof info);
    check("and takes one that arrives while it waits",
          sigtimedwait(&s, &info, &long_wait) == SIGUSR2 && info.si_pid == sender);
    waitpid(sender, NULL, 0);
    sigprocmask(SIG_UNBLOCK, &s, NULL);

    /* A pipe's read, cut short, made again or not. */
    int p[2];
    char c;
    if (pipe(p) == 0) {
        handle(SIGALRM, note, 0);
        seen = 0;
        alarm_ms(50);
        clock_gettime(CLOCK_MONOTONIC, &t0);
        ssize_t r = read(p[0], &c, 1);
        check("a read of an empty pipe is cut short by a handler", r == -1 && errno == EINTR && seen == 1 && elapsed_ms(&t0) < 2000);
        handle(SIGALRM, note, SA_RESTART);
        seen = 0;
        pid_t writer = fork();
        if (writer == 0) {
            usleep(200000);
            write(p[1], "x", 1);
            _exit(0);
        }
        alarm_ms(50);
        r = read(p[0], &c, 1);
        check("and made again after one that asked for that", r == 1 && c == 'x' && seen == 1);
        waitpid(writer, NULL, 0);
        close(p[0]);
        close(p[1]);
    }

    /* A wait for a child, likewise. */
    pid_t child = fork();
    if (child == 0) {
        usleep(400000);
        _exit(5);
    }
    handle(SIGALRM, note, 0);
    seen = 0;
    alarm_ms(50);
    int st = 0;
    check("a wait for a child is cut short", waitpid(child, &st, 0) == -1 && errno == EINTR && seen == 1);
    handle(SIGALRM, note, SA_RESTART);
    alarm_ms(50);
    check("and made again", waitpid(child, &st, 0) == child && WIFEXITED(st) && WEXITSTATUS(st) == 5 && seen == 2);
    handle(SIGALRM, SIG_DFL, 0);

    /* Where it was, and changed. */
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = step_over;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    seen = 0;
    poke3((void *)0x1000);
    /* Here at all, the handler moved the thread past the instruction. */
    check("a fault is handed to a handler with where it was, which can say where to go on from",
          seen == 1 && faulted_at == (void *)0x1000 && fault_code == SEGV_MAPERR);
    handle(SIGSEGV, SIG_DFL, 0);

    /* A stack of its own. */
    alt = mmap(NULL, ALT_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    stack_t ss;
    ss.ss_sp = alt;
    ss.ss_size = ALT_SIZE;
    ss.ss_flags = 0;
    check("sigaltstack names a stack", alt != MAP_FAILED && sigaltstack(&ss, NULL) == 0);
    handle(SIGUSR1, on_alt, SA_ONSTACK);
    seen = 0;
    raise(SIGUSR1);
    check("a handler that asks runs on it", seen == 1 && where >= alt && where < alt + ALT_SIZE);
    check("and is told so", reported_on);
    stack_t now;
    check("which it is not, afterwards", sigaltstack(NULL, &now) == 0 && now.ss_sp == alt && !(now.ss_flags & SS_ONSTACK));
    ss.ss_flags = SS_DISABLE;
    sigaltstack(&ss, NULL);
    handle(SIGUSR1, SIG_DFL, 0);
    check("a program that runs out of stack is saved by a handler on another", in_child(overflows, 20000) == 7);

    /* A poll under a mask that lets a waiting signal through. */
    handle(SIGUSR1, note, 0);
    sigemptyset(&s);
    sigaddset(&s, SIGUSR1);
    sigprocmask(SIG_BLOCK, &s, NULL);
    seen = 0;
    raise(SIGUSR1);
    sigset_t none;
    sigemptyset(&none);
    struct timespec ten = {10, 0};
    clock_gettime(CLOCK_MONOTONIC, &t0);
    int r = ppoll(NULL, 0, &ten, &none);
    check("a poll under a mask that lets a waiting signal through ends at once", r == -1 && errno == EINTR && seen == 1 && elapsed_ms(&t0) < 2000);
    sigprocmask(SIG_BLOCK, NULL, &cur);
    check("and the mask is as it was afterwards", sigismember(&cur, SIGUSR1));
    sigprocmask(SIG_UNBLOCK, &s, NULL);
    handle(SIGUSR1, SIG_DFL, 0);

    char said = 0;
    int ended = pipe(still_here) == 0 ? in_child(holds_term, 5000) : 0;
    read(still_here[0], &said, 1);
    check("a signal held back that would end a program waits", said == 'h');
    check("and ends it when it is let through", ended == -SIGTERM);

    printf("handlertest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
