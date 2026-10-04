/* signalfd: signals read from a descriptor, with everything else an event
 * loop waits on.
 *
 * A descriptor read for some signals, held back, is readable while one of
 * them waits and gives it as Linux's signalfd_siginfo; one read takes as
 * many as are waiting; poll and epoll see it; a read waits for one to come;
 * the set can be changed; a forked child reads its own. The layer answered
 * signalfd with ENOSYS.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/signalfd.h>
#include <sys/wait.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static int readable(int fd)
{
    struct pollfd p = {.fd = fd, .events = POLLIN};
    return poll(&p, 1, 0) == 1 && (p.revents & POLLIN);
}

int main(void)
{
    printf("signalfdtest:\n");
    int rt = SIGRTMIN + 1;
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigaddset(&set, rt);
    sigprocmask(SIG_BLOCK, &set, NULL);

    int sfd = signalfd(-1, &set, SFD_NONBLOCK);
    check("a signal descriptor is made", sfd >= 0);
    struct signalfd_siginfo si[4];
    check("with nothing waiting it is not readable, and a read says EAGAIN",
          !readable(sfd) && read(sfd, si, sizeof si) == -1 && errno == EAGAIN);

    kill(getpid(), SIGUSR1);
    check("one of its signals waiting, it is readable", readable(sfd));
    ssize_t n = read(sfd, si, sizeof si);
    check("a read takes it: which, how, and from whom",
          n == (ssize_t)sizeof si[0] && si[0].ssi_signo == SIGUSR1 && si[0].ssi_code == SI_USER &&
              si[0].ssi_pid == (unsigned)getpid() && si[0].ssi_uid == getuid());
    check("and then it is not readable", !readable(sfd));

    for (int i = 1; i <= 3; i++)
        sigqueue(getpid(), rt, (union sigval){.sival_int = 30 + i});
    n = read(sfd, si, sizeof si);
    check("one read takes as many as are waiting, each with its value, in order",
          n == 3 * (ssize_t)sizeof si[0] && si[0].ssi_int == 31 && si[1].ssi_int == 32 && si[2].ssi_int == 33 &&
              si[0].ssi_code == SI_QUEUE);
    char small[16];
    check("a read with no room for one record is EINVAL", read(sfd, small, sizeof small) == -1 && errno == EINVAL);

    /* epoll sees it too. */
    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 77};
    epoll_ctl(ep, EPOLL_CTL_ADD, sfd, &ev);
    struct epoll_event got;
    int before = epoll_wait(ep, &got, 1, 0);
    kill(getpid(), SIGUSR1);
    int after = epoll_wait(ep, &got, 1, 1000);
    check("epoll says it is readable once one waits", before == 0 && after == 1 && got.data.u64 == 77 && (got.events & EPOLLIN));
    read(sfd, si, sizeof si);
    close(ep);

    /* A new set for the same descriptor. */
    sigset_t only;
    sigemptyset(&only);
    sigaddset(&only, rt);
    check("its set is changed", signalfd(sfd, &only, 0) == sfd);
    kill(getpid(), SIGUSR1);
    check("and a signal no longer in it is not read", !readable(sfd) && read(sfd, si, sizeof si) == -1 && errno == EAGAIN);
    sigset_t pending;
    sigpending(&pending);
    check("but is still waiting", sigismember(&pending, SIGUSR1));
    struct timespec none = {0, 0};
    sigtimedwait(&set, NULL, &none);

    /* A forked child's read waits, and takes its own. */
    int blocking = signalfd(-1, &only, 0);
    pid_t c = fork();
    if (c == 0) {
        ssize_t got_n = read(blocking, si, sizeof si);
        _exit(got_n == (ssize_t)sizeof si[0] && si[0].ssi_signo == (unsigned)rt && si[0].ssi_pid == (unsigned)getppid() ? 0 : 1);
    }
    usleep(20000);
    kill(c, rt);
    int status = -1;
    waitpid(c, &status, 0);
    check("a forked child's read waits for a signal, and takes the one raised for it",
          WIFEXITED(status) && WEXITSTATUS(status) == 0);
    close(blocking);

    /* SIGCHLD, read. */
    sigset_t chld;
    sigemptyset(&chld);
    sigaddset(&chld, SIGCHLD);
    sigprocmask(SIG_BLOCK, &chld, NULL);
    int cfd = signalfd(-1, &chld, 0);
    c = fork();
    if (c == 0)
        _exit(5);
    n = read(cfd, si, sizeof si);
    check("SIGCHLD read says which child and its status",
          n == (ssize_t)sizeof si[0] && si[0].ssi_signo == SIGCHLD && si[0].ssi_pid == (unsigned)c && si[0].ssi_status == 5 &&
              si[0].ssi_code == CLD_EXITED);
    waitpid(c, &status, 0);
    close(cfd);
    close(sfd);

    printf("signalfdtest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
