/* epoll over everything.
 *
 * One set watches a descriptor of every kind a program has: a pipe, a
 * counter, a timer, a signal descriptor fed by a queued signal and by a
 * POSIX timer, a local socket listening by a name, an inotify instance, and
 * another set — and each is reported by its own token when it is ready. An
 * edge-triggered watch is reported when something comes, and not again
 * until more does; a one-shot once, until it is modified; EPOLLRDHUP with a
 * hangup when the other end has gone. What Linux refuses is refused with
 * Linux's errors: a file and memory EPERM, the set itself EINVAL, a set
 * that would watch itself through another ELOOP, twice EEXIST, what is not
 * watched ENOENT. The layer reported an edge at every wait and refused a
 * set in a set with EINVAL, and EINVAL was every error it had.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/inotify.h>
#include <sys/mman.h>
#include <sys/signalfd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/timerfd.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#define NAME "/tmp/epolltest.sock"
#define DIR "/tmp/epolltest.d"

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static int watch(int ep, int fd, unsigned events, uint64_t token)
{
    struct epoll_event e;
    memset(&e, 0, sizeof e);
    e.events = events;
    e.data.u64 = token;
    return epoll_ctl(ep, EPOLL_CTL_ADD, fd, &e);
}

static int change(int ep, int fd, unsigned events, uint64_t token)
{
    struct epoll_event e;
    memset(&e, 0, sizeof e);
    e.events = events;
    e.data.u64 = token;
    return epoll_ctl(ep, EPOLL_CTL_MOD, fd, &e);
}

/* The events reported for `token` by one wait of `ms`, or 0. */
static unsigned reported(int ep, uint64_t token, int ms)
{
    struct epoll_event out[16];
    int n = epoll_wait(ep, out, 16, ms);
    for (int i = 0; i < n; i++)
        if (out[i].data.u64 == token)
            return out[i].events;
    return 0;
}

int main(void)
{
    printf("epolltest:\n");
    unlink(NAME);
    unlink(DIR "/made");
    rmdir(DIR);
    mkdir(DIR, 0755);

    int ep = epoll_create1(EPOLL_CLOEXEC);
    check("a set is made, close-on-exec as asked", ep >= 0 && (fcntl(ep, F_GETFD) & FD_CLOEXEC));

    /* One of every kind. */
    int p[2], q[2];
    pipe(p);
    pipe(q);
    int counter = eventfd(0, EFD_NONBLOCK);
    int timer = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    int queued_sig = SIGRTMIN + 1, timer_sig = SIGRTMIN + 2;
    sigset_t sigs;
    sigemptyset(&sigs);
    sigaddset(&sigs, queued_sig);
    sigaddset(&sigs, timer_sig);
    sigprocmask(SIG_BLOCK, &sigs, NULL);
    int sigfd = signalfd(-1, &sigs, SFD_NONBLOCK);
    int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
    struct sockaddr_un name;
    memset(&name, 0, sizeof name);
    name.sun_family = AF_UNIX;
    strcpy(name.sun_path, NAME);
    bind(listener, (struct sockaddr *)&name, sizeof name);
    listen(listener, 4);
    int notify = inotify_init1(IN_NONBLOCK);
    inotify_add_watch(notify, DIR, IN_CREATE);
    int inner = epoll_create1(0);
    watch(inner, q[0], EPOLLIN, 99);

    int fds[] = {p[0], counter, timer, sigfd, listener, notify, inner};
    const char *what[] = {"a pipe", "a counter", "a timer", "signals", "a listener", "inotify", "a set"};
    int added = 1;
    for (int i = 0; i < 7; i++)
        added &= fds[i] >= 0 && watch(ep, fds[i], EPOLLIN, 1u << i) == 0;
    check("one set watches a descriptor of every kind", added);
    check("with nothing ready, nothing is reported", epoll_wait(ep, (struct epoll_event[4]){0}, 4, 0) == 0);

    /* Each made ready. */
    write(p[1], "x", 1);
    eventfd_write(counter, 1);
    /* Long enough after everything else that a wait has to wait for it. */
    struct itimerspec soon, later;
    memset(&soon, 0, sizeof soon);
    soon.it_value.tv_nsec = 1000000;
    later = soon;
    later.it_value.tv_nsec = 50000000;
    timerfd_settime(timer, 0, &later, NULL);
    sigqueue(getpid(), queued_sig, (union sigval){.sival_int = 5});
    struct sigevent sev;
    memset(&sev, 0, sizeof sev);
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = timer_sig;
    timer_t posix;
    timer_create(CLOCK_MONOTONIC, &sev, &posix);
    timer_settime(posix, 0, &soon, NULL);
    int client = socket(AF_UNIX, SOCK_STREAM, 0);
    connect(client, (struct sockaddr *)&name, sizeof name);
    close(open(DIR "/made", O_CREAT | O_WRONLY, 0644));
    write(q[1], "y", 1);

    /* For two seconds at most. What is ready already is reported at every
     * wait, and nothing here reads it, so a wait does not wait: a count of
     * waits was a few milliseconds, and the timer, or the file server
     * saying what it had seen, came after it. */
    unsigned seen = 0;
    struct timespec began, now;
    clock_gettime(CLOCK_MONOTONIC, &began);
    do {
        struct epoll_event out[16];
        int n = epoll_wait(ep, out, 16, 50);
        for (int i = 0; i < n; i++)
            if ((out[i].events & EPOLLIN) && out[i].data.u64 < 0x80)
                seen |= (unsigned)out[i].data.u64;
        clock_gettime(CLOCK_MONOTONIC, &now);
    } while (seen != 0x7F && (now.tv_sec - began.tv_sec) * 1000 + (now.tv_nsec - began.tv_nsec) / 1000000 < 2000);
    for (int i = 0; i < 7; i++) {
        char line[64];
        snprintf(line, sizeof line, "%s is reported, by its own token", what[i]);
        check(line, seen & (1u << i));
    }
    /* The timer's signal may come a moment after the descriptor's timer. */
    int queued = 0, timed = 0;
    for (int round = 0; round < 40 && !(queued && timed); round++) {
        struct signalfd_siginfo got[4];
        ssize_t n = read(sigfd, got, sizeof got);
        for (int i = 0; i < (int)(n / (ssize_t)sizeof got[0]); i++) {
            queued |= got[i].ssi_signo == (unsigned)queued_sig && got[i].ssi_code == SI_QUEUE && got[i].ssi_int == 5;
            timed |= got[i].ssi_signo == (unsigned)timer_sig && got[i].ssi_code == SI_TIMER;
        }
        if (!(queued && timed))
            usleep(5000);
    }
    check("the signals were the queued one and the POSIX timer's", queued && timed);
    timer_delete(posix);

    /* Edge-triggered. */
    int e[2];
    pipe(e);
    watch(ep, e[0], EPOLLIN | EPOLLET, 100);
    write(e[1], "a", 1);
    check("an edge is reported when something comes", reported(ep, 100, 0) & EPOLLIN);
    check("and not again while nothing more comes, though it is unread", !reported(ep, 100, 0));
    write(e[1], "b", 1);
    check("more comes, and it is reported again", reported(ep, 100, 0) & EPOLLIN);
    check("a level watch is reported at every wait", (reported(ep, 1, 0) & EPOLLIN) && (reported(ep, 1, 0) & EPOLLIN));

    /* One-shot. */
    int o[2];
    pipe(o);
    watch(ep, o[0], EPOLLIN | EPOLLONESHOT, 200);
    write(o[1], "a", 1);
    check("a one-shot is reported", reported(ep, 200, 0) & EPOLLIN);
    write(o[1], "b", 1);
    check("and then not, whatever comes", !reported(ep, 200, 0));
    check("until it is modified", change(ep, o[0], EPOLLIN | EPOLLONESHOT, 200) == 0 && (reported(ep, 200, 0) & EPOLLIN));

    /* The other end gone. */
    int sp[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sp);
    watch(ep, sp[0], EPOLLIN | EPOLLRDHUP, 300);
    close(sp[1]);
    unsigned gone = reported(ep, 300, 100);
    check("the other end gone is EPOLLRDHUP, with EPOLLHUP", (gone & EPOLLRDHUP) && (gone & EPOLLHUP));

    /* What is refused. */
    struct epoll_event any = {.events = EPOLLIN};
    check("watching it twice is EEXIST", epoll_ctl(ep, EPOLL_CTL_ADD, p[0], &any) == -1 && errno == EEXIST);
    check("changing what is not watched is ENOENT", epoll_ctl(ep, EPOLL_CTL_MOD, e[1], &any) == -1 && errno == ENOENT);
    check("removing it is ENOENT", epoll_ctl(ep, EPOLL_CTL_DEL, e[1], NULL) == -1 && errno == ENOENT);
    int file = open("/etc/passwd", O_RDONLY);
    check("a file is EPERM", epoll_ctl(ep, EPOLL_CTL_ADD, file, &any) == -1 && errno == EPERM);
    int memory = memfd_create("epolltest", 0);
    check("memory is EPERM", epoll_ctl(ep, EPOLL_CTL_ADD, memory, &any) == -1 && errno == EPERM);
    check("the set itself is EINVAL", epoll_ctl(ep, EPOLL_CTL_ADD, ep, &any) == -1 && errno == EINVAL);
    check("a set that would watch itself through another is ELOOP",
          epoll_ctl(inner, EPOLL_CTL_ADD, ep, &any) == -1 && errno == ELOOP);
    check("what is not a set is EINVAL", epoll_ctl(p[0], EPOLL_CTL_ADD, q[0], &any) == -1 && errno == EINVAL);
    close(file);
    check("what is not there is EBADF", epoll_ctl(ep, EPOLL_CTL_ADD, file, &any) == -1 && errno == EBADF);
    check("removing what is watched is done", epoll_ctl(ep, EPOLL_CTL_DEL, p[0], NULL) == 0);

    close(memory);
    close(ep);
    close(inner);
    unlink(NAME);
    unlink(DIR "/made");
    rmdir(DIR);
    printf("epolltest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
