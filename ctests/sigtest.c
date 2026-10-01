// LINK: -lutil
/* Signals, as far as a shell and what it runs need them.
 *
 * Quark's kernel runs no handler. It ends a program that has said nothing
 * about a signal, and tells one that has a handler — which the C layer then
 * calls, on the way out of whatever system call the program makes next, or of
 * the wait the signal cut short. This is everything a program can see of
 * that: a handler runs when it should and not before, the three waits a
 * program sits in at a prompt come back with EINTR, a child ends with the
 * signal's number, an exec keeps what was ignored, a pipe nobody reads is
 * SIGPIPE, and Ctrl-C at a terminal reaches what is running in it.
 *
 * Run with an argument it is the other half of one of its own checks: the
 * program something else exec'd.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <pty.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

/* Where the image is staged, which is what this program execs to become
   its own other half. */
#define SELF "/usr/bin/sigtest"

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static volatile sig_atomic_t seen;
static volatile sig_atomic_t last;

static void note(int sig) {
    seen++;
    last = sig;
}

static sigjmp_buf out;

static void leap(int sig) {
    (void)sig;
    siglongjmp(out, 1);
}

static long elapsed_ms(const struct timespec *since) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - since->tv_sec) * 1000 + (now.tv_nsec - since->tv_nsec) / 1000000;
}

/* A child that signals this program after a while: the only way anything
   arrives from outside while this one is waiting. */
static pid_t later(int ms, int sig) {
    pid_t parent = getpid();
    pid_t pid = fork();
    if (pid == 0) {
        usleep(ms * 1000);
        kill(parent, sig);
        _exit(0);
    }
    return pid;
}

static void reap(pid_t pid) {
    int status;
    waitpid(pid, &status, 0);
}

/* How a child ended: the signal's number, negative, or its exit status. */
static int ended(pid_t pid) {
    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        return 1000;
    }
    return WIFSIGNALED(status) ? -WTERMSIG(status) : WEXITSTATUS(status);
}

/* The program an exec'd child becomes: says what it was started doing about
   two signals. */
static int after_exec(void) {
    int ignored = signal(SIGUSR2, SIG_IGN) == SIG_IGN;
    int forgotten = signal(SIGUSR1, SIG_DFL) == SIG_DFL;
    return (ignored ? 0 : 1) | (forgotten ? 0 : 2);
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "after-exec")) {
        return after_exec();
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("signals:\n");
    struct timespec t0;

    /* A handler, and the two ways a program signals itself. Both have run it
       by the time they return. */
    struct sigaction sa = { .sa_handler = note };
    struct sigaction old;
    check("sigaction installs a handler", sigaction(SIGUSR1, &sa, &old) == 0);
    check("and says there was none", old.sa_handler == SIG_DFL);
    check("and what it is now", sigaction(SIGUSR1, NULL, &old) == 0 && old.sa_handler == note);
    check("raise runs it before returning", raise(SIGUSR1) == 0 && seen == 1 && last == SIGUSR1);
    check("so does kill at oneself", kill(getpid(), SIGUSR1) == 0 && seen == 2);

    /* Blocked, it waits. */
    sigset_t set, was, pend;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    check("a signal can be blocked", sigprocmask(SIG_BLOCK, &set, &was) == 0);
    raise(SIGUSR1);
    check("and then does not run", seen == 2);
    check("but is pending", sigpending(&pend) == 0 && sigismember(&pend, SIGUSR1));
    check("until it is unblocked", sigprocmask(SIG_SETMASK, &was, NULL) == 0 && seen == 3);

    /* Ignored, it is nothing. */
    signal(SIGUSR2, SIG_IGN);
    check("an ignored signal does nothing", raise(SIGUSR2) == 0 && seen == 3);

    /* A handler may leave by longjmp, and the mask it ran under goes with
       what sigsetjmp saved. */
    signal(SIGUSR2, leap);
    if (sigsetjmp(out, 1) == 0) {
        raise(SIGUSR2);
        check("a handler that jumps does not return", 0);
    } else {
        sigprocmask(SIG_SETMASK, NULL, &was);
        check("a handler may leave by siglongjmp", 1);
        check("and the mask is as it was", !sigismember(&was, SIGUSR2));
    }
    signal(SIGUSR2, SIG_IGN);

    /* Nothing said, and a signal ends the program: its parent is told which. */
    pid_t pid = fork();
    if (pid == 0) {
        pause();
        _exit(0);
    }
    usleep(50000);
    check("kill reaches another program", kill(pid, SIGTERM) == 0);
    check("which ends with the signal's number", ended(pid) == -SIGTERM);
    pid = fork();
    if (pid == 0) {
        signal(SIGTERM, SIG_IGN);
        signal(SIGINT, note);
        for (;;) {
            pause();
        }
    }
    usleep(50000);
    kill(pid, SIGTERM);
    kill(pid, SIGINT);
    usleep(50000);
    check("a program that ignores or handles one goes on", kill(pid, 0) == 0);
    kill(pid, SIGKILL);
    check("and nothing ignores 9", ended(pid) == -SIGKILL);
    check("a signal for nobody is ESRCH", kill(pid, 0) == -1 && errno == ESRCH);
    pid = fork();
    if (pid == 0) {
        abort();
    }
    check("abort is SIGABRT", ended(pid) == -SIGABRT);

    /* A handler ends pause, in the program that was waiting in it. */
    int ready[2];
    pipe(ready);
    pid = fork();
    if (pid == 0) {
        seen = 0;
        sigaction(SIGTERM, &sa, NULL);
        write(ready[1], "r", 1);
        int r = pause();
        _exit(r == -1 && errno == EINTR && seen == 1 && last == SIGTERM ? 7 : 8);
    }
    char c;
    read(ready[0], &c, 1);
    usleep(50000);
    kill(pid, SIGTERM);
    check("a handler ends pause", ended(pid) == 7);

    /* The waits a signal cuts short. Each says EINTR, soon after the signal
       and long before its own time was up. */
    seen = 0;
    pid = later(100, SIGUSR1);
    struct timespec want = { .tv_sec = 3 }, left = { 0 };
    clock_gettime(CLOCK_MONOTONIC, &t0);
    int r = nanosleep(&want, &left);
    check("a sleep is cut short", r == -1 && errno == EINTR && seen == 1 && elapsed_ms(&t0) < 2000);
    check("and says how much was left", left.tv_sec >= 1);
    reap(pid);

    pid = later(100, SIGUSR1);
    clock_gettime(CLOCK_MONOTONIC, &t0);
    r = poll(NULL, 0, 3000);
    check("so is a poll", r == -1 && errno == EINTR && seen == 2 && elapsed_ms(&t0) < 2000);
    reap(pid);

    int master = -1, slave = -1;
    if (openpty(&master, &slave, NULL, NULL, NULL) != 0) {
        printf("sigtest: openpty: %s\n", strerror(errno));
        return 1;
    }
    char line[64];
    pid = later(100, SIGUSR1);
    clock_gettime(CLOCK_MONOTONIC, &t0);
    r = (int)read(slave, line, sizeof line);
    check("and a read of a terminal", r == -1 && errno == EINTR && seen == 3 && elapsed_ms(&t0) < 2000);
    reap(pid);

    /* Unless the handler asked for the call to go on. */
    sa.sa_flags = SA_RESTART;
    sigaction(SIGUSR1, &sa, NULL);
    pid_t parent = getpid();
    pid = fork();
    if (pid == 0) {
        usleep(100000);
        kill(parent, SIGUSR1);
        usleep(100000);
        write(master, "typed\n", 6);
        _exit(0);
    }
    r = (int)read(slave, line, sizeof line);
    check("SA_RESTART lets the read finish", r == 6 && !memcmp(line, "typed\n", 6) && seen == 4);
    reap(pid);
    sa.sa_flags = 0;
    sigaction(SIGUSR1, &sa, NULL);

    /* A pipe nobody reads. Ignored, the write says EPIPE; with nothing said
       it is the end of the writer, which is how `yes | head` stops. */
    int p[2];
    pipe(p);
    close(p[0]);
    signal(SIGPIPE, SIG_IGN);
    check("a write nobody will read is EPIPE", write(p[1], "x", 1) == -1 && errno == EPIPE);
    signal(SIGPIPE, SIG_DFL);
    pid = fork();
    if (pid == 0) {
        write(p[1], "x", 1);
        _exit(0);
    }
    check("and SIGPIPE for a program that has said nothing", ended(pid) == -SIGPIPE);
    close(p[1]);

    /* An exec keeps what is ignored and forgets handlers: the first is how a
       shell starts a job that Ctrl-C does not reach. */
    pid = fork();
    if (pid == 0) {
        execl(SELF, SELF, "after-exec", (char *)NULL);
        _exit(100);
    }
    check("an exec keeps what is ignored and forgets handlers", ended(pid) == 0);

    /* Ctrl-C. Typed at a terminal, it is for whatever holds the terminal:
       one program that has said nothing, and one reading it with a handler. */
    int both[2];
    pipe(both);
    pid_t idle = fork();
    if (idle == 0) {
        close(master);
        pause();
        _exit(0);
    }
    pid_t reader = fork();
    if (reader == 0) {
        close(master);
        seen = 0;
        /* sigaction and not signal: the C library's `signal` asks for the
           call to be restarted, and this read would go on waiting. */
        sigaction(SIGINT, &sa, NULL);
        write(both[1], "r", 1);
        r = (int)read(slave, line, sizeof line);
        _exit(r == -1 && errno == EINTR && seen == 1 ? 9 : 10);
    }
    /* This program must not be one of them. */
    close(slave);
    read(both[0], &c, 1);
    usleep(50000);
    write(master, "\003", 1);
    check("Ctrl-C ends a program that said nothing", ended(idle) == -SIGINT);
    check("and interrupts one that is reading the terminal", ended(reader) == 9);
    close(master);

    printf("sigtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
