/* Job control: process groups, sessions, programs that stop and are started
 * again, and which group a terminal's typing is for.
 *
 * It is what a shell does, without the shell: a session leader on a terminal
 * of its own, starting jobs in groups of their own, putting one in front,
 * hearing that one has stopped. Everything here is true of Linux, and was
 * checked there first. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pty.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static volatile sig_atomic_t children;

static void on_child(int sig) {
    (void)sig;
    children++;
}

/* A program that does nothing until it is told to stop. */
static void idle(void) {
    for (;;) {
        pause();
    }
}

/* What the session leader found, a bit for each thing. */
enum {
    LED = 1,          /* began a session, and leads it */
    TOOK = 2,         /* took the terminal, and is in front of it */
    NOT_STOPPED = 4,  /* SIGTSTP did not stop a group nobody would continue */
    TTIN = 8,         /* a job reading from behind was stopped for it */
    WROTE = 16,       /* and was allowed to write from there */
    FRONT = 32,       /* brought forward and continued, it read what was typed */
    TSTP = 64,        /* Ctrl-Z stopped the job in front, and not the leader */
    INT = 128,        /* Ctrl-C ended the job in front, and not the leader */
    TTOU = 256,       /* a job that put itself in front was stopped for it */
    BACK = 512,       /* and the leader took the terminal back */
    LEFT = 1024,      /* a job was left stopped when the leader ended */
};

/* The session leader: what a shell with job control is. `slave` is the
   terminal; a byte on `ready` asks the test to type the next thing; `held`
   is a descriptor the last job keeps open for as long as it lives. */
static int lead(int slave, int ready, int held) {
    int found = 0;
    int st = 0;
    char c[8];

    /* The test's handler came with the fork, and would end every wait
       below early. */
    signal(SIGCHLD, SIG_DFL);
    if (setsid() == getpid() && getsid(0) == getpid() && getpgrp() == getpid()) {
        found |= LED;
    }
    if (ioctl(slave, TIOCSCTTY, 0) == 0 && tcgetpgrp(slave) == getpgrp() &&
        tcgetsid(slave) == getpid()) {
        found |= TOOK;
    }
    /* This group's only member has a parent in another session: nobody
       would start it again, so the terminal's stop signals do not stop it. */
    signal(SIGTSTP, SIG_DFL);
    raise(SIGTSTP);
    found |= NOT_STOPPED;
    /* A shell ignores these two, which is what lets it take a terminal back
       from behind. */
    signal(SIGTTOU, SIG_IGN);
    signal(SIGTTIN, SIG_IGN);

    /* A job in the background that reads the terminal. */
    pid_t job = fork();
    if (job == 0) {
        signal(SIGTTOU, SIG_DFL);
        signal(SIGTTIN, SIG_DFL);
        setpgid(0, 0);
        /* Writing from behind is allowed: nothing has asked for it not to be. */
        int wrote = write(slave, "bg\n", 3) == 3;
        long got = read(slave, c, sizeof c);
        _exit(got == 3 && !memcmp(c, "hi\n", 3) ? (wrote ? 0 : 2) : 1);
    }
    setpgid(job, job);
    if (waitpid(job, &st, WUNTRACED) == job && WIFSTOPPED(st) && WSTOPSIG(st) == SIGTTIN) {
        found |= TTIN;
    }
    /* Brought forward and continued: `fg`. */
    int forward = tcsetpgrp(slave, job) == 0 && tcgetpgrp(slave) == job;
    kill(-job, SIGCONT);
    write(ready, "1", 1); /* type "hi" */
    if (waitpid(job, &st, 0) == job && WIFEXITED(st) && forward) {
        if (WEXITSTATUS(st) == 0) {
            found |= FRONT | WROTE;
        } else if (WEXITSTATUS(st) == 2) {
            found |= FRONT;
        }
    }

    /* A job in front, and Ctrl-Z. */
    job = fork();
    if (job == 0) {
        signal(SIGTTOU, SIG_DFL);
        signal(SIGTTIN, SIG_DFL);
        setpgid(0, 0);
        idle();
    }
    setpgid(job, job);
    tcsetpgrp(slave, job);
    write(ready, "2", 1); /* type Ctrl-Z */
    if (waitpid(job, &st, WUNTRACED) == job && WIFSTOPPED(st) && WSTOPSIG(st) == SIGTSTP) {
        found |= TSTP;
    }
    /* Continued in front again, and Ctrl-C. */
    kill(-job, SIGCONT);
    write(ready, "3", 1); /* type Ctrl-C */
    if (waitpid(job, &st, 0) == job && WIFSIGNALED(st) && WTERMSIG(st) == SIGINT) {
        found |= INT;
    }

    /* The job has gone and its group is still in front: this is behind. A
       job that tries to put itself in front from behind is stopped for it. */
    job = fork();
    if (job == 0) {
        signal(SIGTTOU, SIG_DFL);
        setpgid(0, 0);
        tcsetpgrp(slave, getpgrp());
        _exit(0);
    }
    setpgid(job, job);
    if (waitpid(job, &st, WUNTRACED) == job && WIFSTOPPED(st) && WSTOPSIG(st) == SIGTTOU) {
        found |= TTOU;
    }
    kill(job, SIGKILL);
    waitpid(job, &st, 0);
    /* And the shell, which ignores the signal, is not: it takes it back. */
    if (tcsetpgrp(slave, getpgrp()) == 0 && tcgetpgrp(slave) == getpgrp()) {
        found |= BACK;
    }

    /* A job left stopped when its shell ends. There will be nobody to start
       it again, so it is hung up on: SIGHUP, which ends it, and SIGCONT so
       that it hears. It holds `held` open until then, and nothing else. */
    job = fork();
    if (job == 0) {
        close(ready);
        close(slave);
        setpgid(0, 0);
        raise(SIGSTOP);
        _exit(0);
    }
    setpgid(job, job);
    if (waitpid(job, &st, WUNTRACED) == job && WIFSTOPPED(st) && WSTOPSIG(st) == SIGSTOP) {
        found |= LEFT;
    }
    (void)held;
    return found;
}

int main(void) {
    printf("job control:\n");
    int status = 0;

    /* If something that should have gone on is stopped instead, this ends
       with it rather than waiting for ever. */
    alarm(30);

    check("a process is in a group and a session",
          getpgrp() > 0 && getsid(0) > 0 && getpgid(0) == getpgrp());

    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_child;
    sigaction(SIGCHLD, &sa, 0);

    pid_t child = fork();
    if (child == 0) {
        idle();
    }
    check("a child begins in its parent's group", getpgid(child) == getpgrp());
    check("and is put in one of its own", setpgid(child, child) == 0 && getpgid(child) == child);
    errno = 0;
    check("a group that is not there is refused",
          setpgid(child, 0x7fff0000) == -1 && errno == EPERM);
    errno = 0;
    check("and so is moving a process that is nobody's child", setpgid(1, 0) == -1 && errno == ESRCH);

    children = 0;
    check("SIGSTOP stops it, and a wait that asks hears of it",
          kill(child, SIGSTOP) == 0 && waitpid(child, &status, WUNTRACED) == child &&
              WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP);
    /* The signal follows the stop; a wait that found the child already
       stopped can be back before it. */
    for (int i = 0; i < 100 && !children; i++) {
        usleep(10 * 1000);
    }
    check("its parent heard SIGCHLD", children >= 1);
    check("a wait that does not ask hears nothing", waitpid(child, &status, WNOHANG) == 0);
    check("SIGCONT starts it, and a wait that asks hears that",
          kill(child, SIGCONT) == 0 && waitpid(child, &status, WCONTINUED) == child &&
              WIFCONTINUED(status));

    pid_t second = fork();
    if (second == 0) {
        idle();
    }
    check("a second child joins the first one's group",
          setpgid(second, child) == 0 && getpgid(second) == child);
    check("a signal for the group", killpg(child, SIGTERM) == 0);
    int collected = 0;
    for (int i = 0; i < 2; i++) {
        pid_t got = waitpid(-child, &status, 0);
        if ((got == child || got == second) && WIFSIGNALED(status) && WTERMSIG(status) == SIGTERM) {
            collected++;
        }
    }
    check("ends both, and a wait for the group collects them", collected == 2);
    errno = 0;
    check("a group with nobody in it is not there to signal",
          killpg(child, 0) == -1 && errno == ESRCH);
    errno = 0;
    check("or to wait for", waitpid(-child, &status, WNOHANG) == -1 && errno == ECHILD);

    /* A terminal, and a session on it. */
    int master = -1, slave = -1;
    int ready[2], result[2];
    check("a terminal", openpty(&master, &slave, NULL, NULL, NULL) == 0);
    errno = 0;
    check("which is not this session's: there is no group in front to ask about",
          tcgetpgrp(slave) == -1 && errno == ENOTTY);
    if (pipe(ready) || pipe(result)) {
        check("pipes", 0);
        return 1;
    }
    pid_t leader = fork();
    if (leader == 0) {
        close(master);
        close(ready[0]);
        close(result[0]);
        int found = lead(slave, ready[1], result[1]);
        write(result[1], &found, sizeof found);
        _exit(0);
    }
    close(ready[1]);
    close(result[1]);
    /* Type what the leader asks for, when it asks. */
    char stage;
    while (read(ready[0], &stage, 1) == 1) {
        /* Not at once: what is typed is for whoever is in front and
           waiting, and the job has to have got that far. */
        usleep(200 * 1000);
        if (stage == '1') {
            write(master, "hi\n", 3);
        } else if (stage == '2') {
            write(master, "\x1a", 1);
        } else if (stage == '3') {
            write(master, "\x03", 1);
        }
    }
    int found = 0;
    int got = read(result[0], &found, sizeof found) == sizeof found;
    check("the session's leader finished", got && waitpid(leader, &status, 0) == leader);
    check("a process begins a session, and leads it", found & LED);
    check("and takes a terminal as the session's, with itself in front", found & TOOK);
    check("a group nobody would continue is not stopped by SIGTSTP", found & NOT_STOPPED);
    check("a job that reads the terminal from behind is stopped for it", found & TTIN);
    check("but may write to it", found & WROTE);
    check("brought forward and continued, it reads what is typed", found & FRONT);
    check("Ctrl-Z stops the job in front", found & TSTP);
    check("and Ctrl-C, when it is continued, ends it", found & INT);
    check("a job that puts itself in front from behind is stopped for that", found & TTOU);
    check("and a shell, which ignores the signal, takes the terminal back", found & BACK);
    /* The leader has ended with a job stopped. The end of this pipe is the
       job's last descriptor closing: it was hung up on, and went. */
    check("a job is left stopped when its shell ends", found & LEFT);
    check("and is hung up on, having nobody to start it again", read(result[0], &stage, 1) == 0);

    printf("jobtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
