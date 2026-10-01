/* Two signals the kernel raises of its own accord: one after a time, and one
 * when a child ends.
 *
 * Nothing stands in for either. A program that waits for a child *or* a
 * time, whichever comes first, has to be woken by the one that came — and
 * here it was woken by neither: there was no alarm to set, and a child
 * ending said nothing to anybody. GNU `timeout` waited for ever, in
 * `sigsuspend`, for a SIGCHLD; a shell's `read -t` never timed out. */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define SELF "/usr/bin/alarmtest"

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static volatile sig_atomic_t alarms, children;

static void on_alarm(int sig) {
    (void)sig;
    alarms++;
}

static void on_child(int sig) {
    (void)sig;
    children++;
}

static struct timespec started;

static void start(void) {
    clock_gettime(CLOCK_MONOTONIC, &started);
}

static long ms(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - started.tv_sec) * 1000 + (now.tv_nsec - started.tv_nsec) / 1000000;
}

int main(int argc, char **argv) {
    /* The far side of an exec: a program that would take ten seconds. */
    if (argc == 2 && !strcmp(argv[1], "sleeper")) {
        sleep(10);
        return 0;
    }

    printf("alarms, and a child ending:\n");
    int status = 0;
    /* A handler that does not ask for the call it interrupts to go on. */
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_alarm;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGALRM, &sa, NULL);

    start();
    check("there is no alarm until one is set", alarm(1) == 0);
    errno = 0;
    int r = pause();
    long took = ms();
    check("an alarm ends a pause", r == -1 && errno == EINTR && alarms == 1);
    check("after its second, and not before", took >= 1000 && took < 1500);

    alarm(10);
    check("turning one off says what was left of it", alarm(0) == 10);
    check("and then there is none", alarm(0) == 0);

    /* One that repeats. */
    struct itimerval every = { .it_interval = { 0, 50000 }, .it_value = { 0, 50000 } };
    struct itimerval now;
    alarms = 0;
    start();
    check("a timer is set", setitimer(ITIMER_REAL, &every, NULL) == 0);
    while (alarms < 4 && ms() < 3000) {
        pause();
    }
    took = ms();
    check("one that repeats is raised again and again", alarms >= 4 && took >= 200 && took < 600);
    check("and says what it repeats at",
          getitimer(ITIMER_REAL, &now) == 0 && now.it_interval.tv_sec == 0
              && now.it_interval.tv_usec == 50000);
    struct itimerval off;
    memset(&off, 0, sizeof off);
    setitimer(ITIMER_REAL, &off, NULL);
    alarms = 0;
    usleep(200 * 1000);
    check("until it is turned off",
          alarms == 0 && getitimer(ITIMER_REAL, &now) == 0 && now.it_value.tv_sec == 0
              && now.it_value.tv_usec == 0);
    errno = 0;
    check("a timer for time spent running is refused",
          setitimer(ITIMER_VIRTUAL, &every, NULL) == -1 && errno == EINVAL);

    /* A sleep that an alarm cut short says how much of it was left. */
    alarms = 0;
    alarm(1);
    unsigned unslept = sleep(3);
    check("an alarm ends a sleep, which says what was left", alarms == 1 && unslept >= 1 && unslept <= 2);

    /* What the signal does to a program that has said nothing. */
    start();
    pid_t child = fork();
    if (child == 0) {
        signal(SIGALRM, SIG_DFL);
        alarm(1);
        sleep(10);
        _exit(0);
    }
    check("an alarm nobody handles ends the program",
          waitpid(child, &status, 0) == child && WIFSIGNALED(status) && WTERMSIG(status) == SIGALRM
              && ms() < 3000);

    /* The alarm is the process's: a program started with one still has it,
       and that is how a program is given a time to finish in. */
    start();
    child = fork();
    if (child == 0) {
        signal(SIGALRM, SIG_DFL);
        alarm(1);
        execl(SELF, "alarmtest", "sleeper", (char *)NULL);
        _exit(2);
    }
    check("an alarm set before an exec is still set after it",
          waitpid(child, &status, 0) == child && WIFSIGNALED(status) && WTERMSIG(status) == SIGALRM
              && ms() < 3000);

    /* And not a child's: a child set none. */
    alarm(30);
    child = fork();
    if (child == 0) {
        _exit(alarm(0) == 0 ? 0 : 1);
    }
    check("a forked child has no alarm of its parent's",
          waitpid(child, &status, 0) == child && WEXITSTATUS(status) == 0);
    check("and the parent still has its own", alarm(0) >= 29);

    /* A child ending. The signals are blocked except while waiting for one,
       which is what keeps one that arrives early from being missed. */
    sa.sa_handler = on_child;
    sigaction(SIGCHLD, &sa, NULL);
    sigset_t block, before;
    sigemptyset(&block);
    sigaddset(&block, SIGCHLD);
    sigaddset(&block, SIGALRM);
    sigprocmask(SIG_BLOCK, &block, &before);

    start();
    child = fork();
    if (child == 0) {
        usleep(100 * 1000);
        _exit(3);
    }
    while (!children && ms() < 3000) {
        sigsuspend(&before);
    }
    check("a child ending wakes a parent that is waiting for a signal", children == 1);
    check("with the child there to collect",
          waitpid(child, &status, WNOHANG) == child && WEXITSTATUS(status) == 3);

    /* What `timeout` does: a child that would take ten seconds, an alarm
       for one, and a wait for whichever signal comes first. */
    sa.sa_handler = on_alarm;
    sigaction(SIGALRM, &sa, NULL);
    alarms = 0;
    children = 0;
    start();
    child = fork();
    if (child == 0) {
        sigprocmask(SIG_SETMASK, &before, NULL);
        sleep(10);
        _exit(0);
    }
    alarm(1);
    int told = 0;
    while (waitpid(child, &status, WNOHANG) == 0 && ms() < 8000) {
        sigsuspend(&before);
        if (alarms && !told) {
            kill(child, SIGTERM);
            told = 1;
        }
    }
    took = ms();
    check("a child is given a second and then ended",
          told && WIFSIGNALED(status) && WTERMSIG(status) == SIGTERM);
    check("which took a second, and not ten", took >= 1000 && took < 3000);
    /* It ended while this was not waiting for a signal, so its SIGCHLD is
       held until the mask lets it in. */
    check("its ending waits to be heard while the signal is blocked", children == 0);
    sigprocmask(SIG_SETMASK, &before, NULL);
    check("and is heard when it is not", children == 1);

    printf("alarmtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
