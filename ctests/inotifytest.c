/* inotify: what a program is told of changes to the files it watches.
 *
 * A directory watched is told what is made in it, opened, read, written,
 * closed, given new attributes, renamed within it — the two halves tied by
 * one cookie — and removed, each by its name; a file watched is told of
 * itself, and of its end; a watch removed says so, and one that was to be
 * told once is told once; a read with no room for an event is refused, one
 * that may not wait is told so and one that may waits for the next; poll
 * says when there is something, and FIONREAD how much; the same event twice
 * running is said once; and a queue with no room left says so, once. The
 * layer answered inotify_init1 with ENOSYS.
 *
 * What closes a file is the kernel's, and the file server hears of it after
 * close has returned: so each step waits for the event that ends it.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define DIR "/tmp/inotifytest.d"

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

struct ev {
    int wd;
    unsigned mask, cookie;
    char name[64];
};

static struct ev evs[1024];
static int nevs;

/* Read what waits on a descriptor that may not wait, after what was read
   before; the bytes read. */
static long take(int fd)
{
    static char buf[16384] __attribute__((aligned(8)));
    long total = 0;
    for (;;) {
        ssize_t n = read(fd, buf, sizeof buf);
        if (n <= 0)
            return total;
        total += n;
        for (char *p = buf; p < buf + n;) {
            struct inotify_event *e = (struct inotify_event *)p;
            if (nevs < 1024) {
                evs[nevs].wd = e->wd;
                evs[nevs].mask = e->mask;
                evs[nevs].cookie = e->cookie;
                snprintf(evs[nevs].name, sizeof evs[nevs].name, "%s", e->len ? e->name : "");
                nevs++;
            }
            p += sizeof *e + e->len;
        }
    }
}

/* Where the first event like this one is among those read, or -1. */
static int at(int wd, unsigned mask, const char *name)
{
    for (int i = 0; i < nevs; i++)
        if (evs[i].wd == wd && evs[i].mask == mask && strcmp(evs[i].name, name) == 0)
            return i;
    return -1;
}

static int count(int wd, unsigned mask, const char *name)
{
    int n = 0;
    for (int i = 0; i < nevs; i++)
        n += evs[i].wd == wd && evs[i].mask == mask && strcmp(evs[i].name, name) == 0;
    return n;
}

/* Begin a step: nothing read yet. */
static void step(void)
{
    nevs = 0;
}

/* Read until an event like this one has come, or two seconds have gone. */
static int until(int fd, int wd, unsigned mask, const char *name)
{
    for (int i = 0; i < 40 && at(wd, mask, name) < 0; i++) {
        struct pollfd p = {fd, POLLIN, 0};
        poll(&p, 1, 50);
        take(fd);
    }
    return at(wd, mask, name) >= 0;
}

static void make(const char *path)
{
    int f = open(path, O_CREAT | O_WRONLY, 0644);
    if (f >= 0)
        close(f);
}

static long ms_since(const struct timespec *t0)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (t.tv_sec - t0->tv_sec) * 1000 + (t.tv_nsec - t0->tv_nsec) / 1000000;
}

int main(void)
{
    printf("inotifytest:\n");
    unlink(DIR "/a");
    unlink(DIR "/b");
    unlink(DIR "/c");
    unlink(DIR "/late");
    unlink(DIR "/a-name-long-enough-to-need-more-room");
    rmdir(DIR "/sub");
    rmdir(DIR);
    mkdir(DIR, 0755);

    int fd = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    check("an instance is made", fd >= 0);
    check("close-on-exec, as asked", fd >= 0 && (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0);
    char buf[64];
    check("a read that may not wait, with nothing to read, is EAGAIN",
          read(fd, buf, sizeof buf) == -1 && errno == EAGAIN);
    int dw = inotify_add_watch(fd, DIR, IN_ALL_EVENTS);
    check("a directory is watched", dw > 0);
    check("and watching it again is the same watch", dw > 0 && inotify_add_watch(fd, DIR, IN_ALL_EVENTS) == dw);
    struct pollfd p = {fd, POLLIN, 0};
    check("with nothing waiting it is not readable", poll(&p, 1, 0) == 0);

    /* Made, opened, written twice, closed. */
    step();
    int f = open(DIR "/a", O_CREAT | O_WRONLY, 0644);
    check("then something is made in it, and it is readable", poll(&p, 1, 2000) == 1 && (p.revents & POLLIN));
    int queued = -1;
    check("FIONREAD says how much waits", ioctl(fd, FIONREAD, &queued) == 0 && queued >= 32);
    write(f, "hello", 5);
    write(f, "again", 5);
    close(f);
    until(fd, dw, IN_CLOSE_WRITE, "a");
    check("a file made in it is IN_CREATE, by name", at(dw, IN_CREATE, "a") >= 0);
    check("opened, IN_OPEN", at(dw, IN_OPEN, "a") >= 0);
    check("written twice running, one IN_MODIFY", count(dw, IN_MODIFY, "a") == 1);
    check("closed after writing, IN_CLOSE_WRITE", at(dw, IN_CLOSE_WRITE, "a") >= 0);
    check("in the order they happened",
          at(dw, IN_CREATE, "a") < at(dw, IN_OPEN, "a") && at(dw, IN_OPEN, "a") < at(dw, IN_MODIFY, "a") &&
              at(dw, IN_MODIFY, "a") < at(dw, IN_CLOSE_WRITE, "a"));

    /* Read and closed. */
    step();
    f = open(DIR "/a", O_RDONLY);
    read(f, buf, 5);
    close(f);
    until(fd, dw, IN_CLOSE_NOWRITE, "a");
    check("read, IN_ACCESS; closed after only reading, IN_CLOSE_NOWRITE",
          at(dw, IN_ACCESS, "a") >= 0 && at(dw, IN_CLOSE_NOWRITE, "a") > at(dw, IN_ACCESS, "a"));

    step();
    chmod(DIR "/a", 0600);
    check("a change of mode is IN_ATTRIB", until(fd, dw, IN_ATTRIB, "a"));

    /* The file itself. */
    int fw = inotify_add_watch(fd, DIR "/a", IN_MODIFY | IN_ATTRIB | IN_DELETE_SELF | IN_MOVE_SELF);
    check("a file is watched", fw > 0 && fw != dw);
    step();
    f = open(DIR "/a", O_WRONLY | O_APPEND);
    write(f, "x", 1);
    close(f);
    until(fd, dw, IN_CLOSE_WRITE, "a");
    check("a file watched is told of itself, with no name", at(fw, IN_MODIFY, "") >= 0);
    check("and its directory of it, by name", at(dw, IN_MODIFY, "a") >= 0);

    /* A new name. */
    step();
    rename(DIR "/a", DIR "/b");
    until(fd, fw, IN_MOVE_SELF, "");
    int from = at(dw, IN_MOVED_FROM, "a"), to = at(dw, IN_MOVED_TO, "b");
    check("a rename is IN_MOVED_FROM the old name and IN_MOVED_TO the new",
          from >= 0 && to > from);
    check("tied by one cookie", from >= 0 && to >= 0 && evs[from].cookie != 0 && evs[from].cookie == evs[to].cookie);
    check("and the file is told it moved", at(fw, IN_MOVE_SELF, "") >= 0);

    step();
    mkdir(DIR "/sub", 0755);
    check("a directory made in it is IN_CREATE and IN_ISDIR", until(fd, dw, IN_CREATE | IN_ISDIR, "sub"));

    /* The end of the file. */
    step();
    unlink(DIR "/b");
    until(fd, dw, IN_DELETE, "b");
    until(fd, fw, IN_IGNORED, "");
    check("its last name removed is IN_DELETE, by name", at(dw, IN_DELETE, "b") >= 0);
    check("and to its own watch IN_DELETE_SELF, then IN_IGNORED",
          at(fw, IN_DELETE_SELF, "") >= 0 && at(fw, IN_IGNORED, "") > at(fw, IN_DELETE_SELF, ""));
    check("which is then no watch", inotify_rm_watch(fd, fw) == -1 && errno == EINVAL);

    step();
    rmdir(DIR "/sub");
    check("a directory removed is IN_DELETE and IN_ISDIR", until(fd, dw, IN_DELETE | IN_ISDIR, "sub"));

    /* Told once. */
    make(DIR "/c");
    int once = inotify_add_watch(fd, DIR "/c", IN_ATTRIB | IN_ONESHOT);
    step();
    chmod(DIR "/c", 0600);
    chmod(DIR "/c", 0644);
    until(fd, once, IN_IGNORED, "");
    check("a watch to be told once is told once, and then is no more",
          count(once, IN_ATTRIB, "") == 1 && at(once, IN_IGNORED, "") > at(once, IN_ATTRIB, ""));

    /* What is refused. */
    check("IN_ONLYDIR of a file is ENOTDIR",
          inotify_add_watch(fd, DIR "/c", IN_MODIFY | IN_ONLYDIR) == -1 && errno == ENOTDIR);
    check("a path that is not there is ENOENT", inotify_add_watch(fd, DIR "/none", IN_MODIFY) == -1 && errno == ENOENT);
    check("a mask with no event in it is EINVAL", inotify_add_watch(fd, DIR, IN_ONLYDIR) == -1 && errno == EINVAL);
    check("IN_MASK_CREATE of what is watched already is EEXIST",
          inotify_add_watch(fd, DIR, IN_CREATE | IN_MASK_CREATE) == -1 && errno == EEXIST);
    int pipe_ends[2];
    pipe(pipe_ends);
    check("a descriptor that is not an instance is EINVAL",
          inotify_add_watch(pipe_ends[0], DIR, IN_MODIFY) == -1 && errno == EINVAL);
    close(pipe_ends[0]);
    close(pipe_ends[1]);

    /* No room for one event. */
    step();
    make(DIR "/a-name-long-enough-to-need-more-room");
    until(fd, dw, IN_OPEN, "a-name-long-enough-to-need-more-room");
    make(DIR "/a-name-long-enough-to-need-more-room");
    struct pollfd q = {fd, POLLIN, 0};
    poll(&q, 1, 2000);
    check("a read with room for less than the next event is EINVAL",
          read(fd, buf, sizeof(struct inotify_event)) == -1 && errno == EINVAL);
    take(fd);

    /* A watch removed. */
    step();
    check("a watch is removed", inotify_rm_watch(fd, dw) == 0);
    check("and says IN_IGNORED", until(fd, dw, IN_IGNORED, ""));
    check("and a number that is no watch is EINVAL", inotify_rm_watch(fd, dw) == -1 && errno == EINVAL);
    close(fd);

    /* A read that may wait. */
    int bfd = inotify_init();
    int bw = inotify_add_watch(bfd, DIR, IN_CREATE);
    pid_t c = fork();
    if (c == 0) {
        usleep(100000);
        make(DIR "/late");
        _exit(0);
    }
    struct timespec t0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    char big[512] __attribute__((aligned(8)));
    ssize_t n = read(bfd, big, sizeof big);
    long waited = ms_since(&t0);
    struct inotify_event *e = (struct inotify_event *)big;
    check("a read that may wait waits for the next event, and is given it",
          n >= (ssize_t)sizeof *e && waited >= 50 && e->wd == bw && e->mask == IN_CREATE && strcmp(e->name, "late") == 0);
    int status;
    waitpid(c, &status, 0);
    close(bfd);

    /* More than there is room for. */
    int ofd = inotify_init1(IN_NONBLOCK);
    int ow = inotify_add_watch(ofd, DIR, IN_CREATE | IN_DELETE);
    for (int i = 0; i < 400; i++) {
        char name[64];
        snprintf(name, sizeof name, DIR "/f%03d", i);
        make(name);
        unlink(name);
    }
    step();
    take(ofd);
    int overflows = 0;
    for (int i = 0; i < nevs; i++)
        overflows += evs[i].mask == IN_Q_OVERFLOW;
    check("a queue with no room left says IN_Q_OVERFLOW, once, and last",
          nevs > 0 && nevs < 800 && overflows == 1 && evs[nevs - 1].mask == IN_Q_OVERFLOW && evs[nevs - 1].wd == -1);
    step();
    unlink(DIR "/late");
    make(DIR "/late");
    check("and once that is read, events come again",
          until(ofd, ow, IN_CREATE, "late") && at(ow, IN_DELETE, "late") >= 0);
    close(ofd);

    unlink(DIR "/c");
    unlink(DIR "/late");
    unlink(DIR "/a-name-long-enough-to-need-more-room");
    rmdir(DIR);
    printf("inotifytest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
