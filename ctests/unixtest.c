/* Sockets of the local family, by a name: what D-Bus and Wayland are found
 * by.
 *
 * A server binds a path and listens; a client in another program connects
 * by the path and is told who listens; the server accepts and is told who
 * connected; bytes go both ways, several descriptors go in one message and
 * credentials come to a receiver that asked for them; a taken name, a name
 * nothing listens at and an accept with nothing waiting each say what Linux
 * says. The layer answered socket() with EAFNOSUPPORT for every family.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static const char *NAME = "/tmp/unixtest.sock";

static struct sockaddr_un named(void)
{
    struct sockaddr_un a;
    memset(&a, 0, sizeof a);
    a.sun_family = AF_UNIX;
    strcpy(a.sun_path, NAME);
    return a;
}

/* What the child does: connect, say who it found, send three descriptors in
   one message and a line, and read the server's answer. */
static int client(void)
{
    int s = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    struct sockaddr_un a = named();
    if (connect(s, (struct sockaddr *)&a, sizeof a) != 0)
        return 10;
    struct ucred who;
    socklen_t len = sizeof who;
    if (getsockopt(s, SOL_SOCKET, SO_PEERCRED, &who, &len) != 0 || who.pid != getppid() || who.uid != getuid())
        return 11;
    int fds[3] = {open("/etc/hostile-sample", O_RDONLY), open("/dev/null", O_RDONLY), open("/etc/libc.tests", O_RDONLY)};
    char control[CMSG_SPACE(sizeof fds)];
    memset(control, 0, sizeof control);
    struct iovec v = {"three", 5};
    struct msghdr m = {0};
    m.msg_iov = &v;
    m.msg_iovlen = 1;
    m.msg_control = control;
    m.msg_controllen = sizeof control;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof fds);
    memcpy(CMSG_DATA(c), fds, sizeof fds);
    if (sendmsg(s, &m, 0) != 5)
        return 12;
    char back[8];
    ssize_t n = recv(s, back, sizeof back, 0);
    if (n != 2 || memcmp(back, "ok", 2) != 0)
        return 13;
    close(s);
    return 0;
}

int main(void)
{
    printf("unixtest:\n");
    unlink(NAME);
    int l = socket(AF_UNIX, SOCK_STREAM, 0);
    check("a socket of the local family is made", l >= 0);
    struct sockaddr_un a = named();
    check("it is bound to a name", bind(l, (struct sockaddr *)&a, sizeof a) == 0);
    int other = socket(AF_UNIX, SOCK_STREAM, 0);
    check("a name that is taken is EADDRINUSE", bind(other, (struct sockaddr *)&a, sizeof a) == -1 && errno == EADDRINUSE);
    check("a name nothing listens at is ECONNREFUSED",
          connect(other, (struct sockaddr *)&a, sizeof a) == -1 && errno == ECONNREFUSED);
    close(other);
    check("it listens", listen(l, 4) == 0);
    struct sockaddr_un back;
    socklen_t blen = sizeof back;
    check("getsockname says its name", getsockname(l, (struct sockaddr *)&back, &blen) == 0 && strcmp(back.sun_path, NAME) == 0);
    fcntl(l, F_SETFL, O_NONBLOCK);
    check("an accept that may not wait, with nothing waiting, is EAGAIN", accept(l, NULL, NULL) == -1 && errno == EAGAIN);
    fcntl(l, F_SETFL, 0);

    pid_t c = fork();
    if (c == 0)
        _exit(client());
    struct pollfd p = {.fd = l, .events = POLLIN};
    check("the listener is readable when a connection waits", poll(&p, 1, 2000) == 1 && (p.revents & POLLIN));
    int s = accept4(l, NULL, NULL, SOCK_CLOEXEC);
    check("and accept gives the connection", s >= 0);
    struct ucred who;
    socklen_t len = sizeof who;
    check("which says who connected",
          getsockopt(s, SOL_SOCKET, SO_PEERCRED, &who, &len) == 0 && who.pid == c && who.uid == getuid() && who.gid == getgid());
    int on = 1;
    check("and can ask to be told who sent what", setsockopt(s, SOL_SOCKET, SO_PASSCRED, &on, sizeof on) == 0);

    char data[16];
    int fds[4] = {-1, -1, -1, -1};
    char control[CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(sizeof fds)];
    struct iovec v = {data, sizeof data};
    struct msghdr m = {0};
    m.msg_iov = &v;
    m.msg_iovlen = 1;
    m.msg_control = control;
    m.msg_controllen = sizeof control;
    ssize_t n = recvmsg(s, &m, 0);
    int nfds = 0, credited = 0;
    for (struct cmsghdr *h = CMSG_FIRSTHDR(&m); h; h = CMSG_NXTHDR(&m, h)) {
        if (h->cmsg_level == SOL_SOCKET && h->cmsg_type == SCM_RIGHTS) {
            nfds = (int)((h->cmsg_len - CMSG_LEN(0)) / sizeof(int));
            memcpy(fds, CMSG_DATA(h), nfds * sizeof(int));
        }
        if (h->cmsg_level == SOL_SOCKET && h->cmsg_type == SCM_CREDENTIALS) {
            struct ucred cr;
            memcpy(&cr, CMSG_DATA(h), sizeof cr);
            credited = cr.pid == c && cr.uid == getuid();
        }
    }
    check("one message brings its bytes and its three descriptors", n == 5 && memcmp(data, "three", 5) == 0 && nfds == 3);
    char first[8] = {0};
    check("each a descriptor that works", nfds == 3 && read(fds[0], first, 1) == 1 && fcntl(fds[1], F_GETFD) >= 0);
    check("and who sent it, as asked", credited);
    check("send and recv carry bytes the other way", send(s, "ok", 2, 0) == 2);
    int status = -1;
    waitpid(c, &status, 0);
    check("the client found everything it looked for", WIFEXITED(status) && WEXITSTATUS(status) == 0);
    for (int i = 0; i < nfds; i++)
        close(fds[i]);
    close(s);
    close(l);

    int late = socket(AF_UNIX, SOCK_STREAM, 0);
    check("once nothing listens, connecting is ECONNREFUSED",
          connect(late, (struct sockaddr *)&a, sizeof a) == -1 && errno == ECONNREFUSED);
    close(late);
    check("a datagram socket is not one", socket(AF_UNIX, SOCK_DGRAM, 0) == -1 && errno == EPROTONOSUPPORT);
    check("the name is removed like any other", unlink(NAME) == 0);

    printf("unixtest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
