/* Sockets of the network: IPv4's and IPv6's families, over this machine's
 * own addresses.
 *
 * A stream: bound, listened on, connected to without waiting and said to
 * be connected when it is, accepted with the flags asked for, bytes both
 * ways by send and recv and by read and write, peeked at, half closed; a
 * connection refused, one reset, a receive that times out and a blocking
 * accept a signal ends. Datagrams: sent with an address and received with
 * one, connected, cut to the room there is and said to have been. IPv6
 * beside IPv4, an IPv6 socket hearing IPv4 as ::ffff:a.b.c.d unless it
 * asked for IPv6 alone, and a name resolved by the C library's own
 * resolver. The layer answered socket() with EAFNOSUPPORT for every family
 * but the local one.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static struct sockaddr_in v4(const char *addr, int port)
{
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(port);
    inet_pton(AF_INET, addr, &a.sin_addr);
    return a;
}

static struct sockaddr_in6 v6(const char *addr, int port)
{
    struct sockaddr_in6 a;
    memset(&a, 0, sizeof a);
    a.sin6_family = AF_INET6;
    a.sin6_port = htons(port);
    inet_pton(AF_INET6, addr, &a.sin6_addr);
    return a;
}

/* Whether `fd` is ready for `events` within `ms`. */
static int ready(int fd, short events, int ms)
{
    struct pollfd p = {fd, events, 0};
    return poll(&p, 1, ms) == 1 && (p.revents & events);
}

static long since_ms(const struct timespec *t0)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (t.tv_sec - t0->tv_sec) * 1000 + (t.tv_nsec - t0->tv_nsec) / 1000000;
}

static void stream4(void)
{
    int l = socket(AF_INET, SOCK_STREAM, 0);
    check("a stream socket of IPv4's family", l >= 0);
    struct stat st;
    check("which is a socket to fstat", fstat(l, &st) == 0 && S_ISSOCK(st.st_mode));
    struct sockaddr_in at = v4("127.0.0.1", 0);
    check("bound to 127.0.0.1", bind(l, (struct sockaddr *)&at, sizeof at) == 0);
    socklen_t len = sizeof at;
    check("on a port it is told", getsockname(l, (struct sockaddr *)&at, &len) == 0 && at.sin_family == AF_INET &&
                                      at.sin_port != 0 && at.sin_addr.s_addr == htonl(INADDR_LOOPBACK));
    check("and listening", listen(l, 8) == 0);
    int on = 0;
    len = sizeof on;
    check("as it says", getsockopt(l, SOL_SOCKET, SO_ACCEPTCONN, &on, &len) == 0 && on == 1);
    fcntl(l, F_SETFL, O_NONBLOCK);
    check("an accept that may not wait, with nobody there, is EAGAIN", accept(l, NULL, NULL) == -1 && errno == EAGAIN);

    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 7};
    epoll_ctl(ep, EPOLL_CTL_ADD, l, &ev);

    int c = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    int r = connect(c, (struct sockaddr *)&at, sizeof at);
    check("a connection that may not wait is in progress", r == 0 || errno == EINPROGRESS);
    int err = -1;
    len = sizeof err;
    check("and writable when it is made, with no error",
          ready(c, POLLOUT, 2000) && getsockopt(c, SOL_SOCKET, SO_ERROR, &err, &len) == 0 && err == 0);
    struct epoll_event got;
    check("epoll says the listener has a connection", epoll_wait(ep, &got, 1, 2000) == 1 && got.data.u64 == 7);
    struct sockaddr_in from;
    len = sizeof from;
    int s = accept4(l, (struct sockaddr *)&from, &len, SOCK_NONBLOCK | SOCK_CLOEXEC);
    struct sockaddr_in mine;
    socklen_t mlen = sizeof mine;
    check("accepted, from where the client is",
          s >= 0 && getsockname(c, (struct sockaddr *)&mine, &mlen) == 0 && from.sin_family == AF_INET &&
              from.sin_port == mine.sin_port && from.sin_addr.s_addr == mine.sin_addr.s_addr);
    check("with the flags accept4 was given",
          s >= 0 && (fcntl(s, F_GETFL) & O_NONBLOCK) && (fcntl(s, F_GETFD) & FD_CLOEXEC));
    struct sockaddr_in peer;
    len = sizeof peer;
    check("and the client's peer is the listener",
          getpeername(c, (struct sockaddr *)&peer, &len) == 0 && peer.sin_port == at.sin_port);

    char buf[64];
    check("send", send(c, "over lo", 7, 0) == 7);
    check("and recv", ready(s, POLLIN, 2000) && recv(s, buf, sizeof buf, 0) == 7 && memcmp(buf, "over lo", 7) == 0);
    check("a recv with nothing come, on a socket that may not wait, is EAGAIN",
          recv(s, buf, sizeof buf, 0) == -1 && errno == EAGAIN);
    check("and so with MSG_DONTWAIT on one that may", recv(c, buf, sizeof buf, MSG_DONTWAIT) == -1 && errno == EAGAIN);
    check("write and read", write(s, "both ways", 9) == 9 && ready(c, POLLIN, 2000) && read(c, buf, sizeof buf) == 9 &&
                                memcmp(buf, "both ways", 9) == 0);
    int pending = -1;
    write(s, "peek", 4);
    ready(c, POLLIN, 2000);
    check("FIONREAD says what waits", ioctl(c, FIONREAD, &pending) == 0 && pending == 4);
    check("MSG_PEEK leaves it", recv(c, buf, sizeof buf, MSG_PEEK) == 4 && recv(c, buf, sizeof buf, 0) == 4 &&
                                     memcmp(buf, "peek", 4) == 0);
    int nodelay = 1;
    check("TCP_NODELAY is taken, and said", setsockopt(c, IPPROTO_TCP, TCP_NODELAY, &nodelay, sizeof nodelay) == 0 &&
                                                 (nodelay = 0, len = sizeof nodelay,
                                                  getsockopt(c, IPPROTO_TCP, TCP_NODELAY, &nodelay, &len) == 0) &&
                                                 nodelay == 1);
    check("shutting down writing", shutdown(c, SHUT_WR) == 0);
    check("is the end of what the other end reads", ready(s, POLLIN, 2000) && recv(s, buf, sizeof buf, 0) == 0);
    check("while it can still send", send(s, "after", 5, 0) == 5 && ready(c, POLLIN, 2000) &&
                                         recv(c, buf, sizeof buf, 0) == 5);
    close(s);
    check("and its closing is the end of it", ready(c, POLLIN, 2000) && recv(c, buf, sizeof buf, 0) == 0);
    close(c);
    close(ep);

    /* Closed with something unread: a reset, which the other end is told
       once, and then the end. */
    c = socket(AF_INET, SOCK_STREAM, 0);
    connect(c, (struct sockaddr *)&at, sizeof at);
    ready(l, POLLIN, 2000);
    s = accept(l, NULL, NULL);
    send(c, "unread", 6, 0);
    usleep(50000);
    close(s);
    ready(c, POLLIN, 2000);
    errno = 0;
    check("a connection reset is ECONNRESET", recv(c, buf, sizeof buf, 0) == -1 && errno == ECONNRESET);
    signal(SIGPIPE, SIG_IGN);
    check("and a send to it EPIPE", send(c, "x", 1, MSG_NOSIGNAL) == -1 && errno == EPIPE);
    close(c);

    /* A receive that would wait for ever waits as long as it was told. */
    c = socket(AF_INET, SOCK_STREAM, 0);
    connect(c, (struct sockaddr *)&at, sizeof at);
    ready(l, POLLIN, 2000);
    s = accept(l, NULL, NULL);
    struct timeval tv = {0, 200000};
    setsockopt(c, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
    struct timespec t0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    long n = recv(c, buf, sizeof buf, 0);
    long took = since_ms(&t0);
    check("SO_RCVTIMEO: a receive gives up when it says, with EAGAIN", n == -1 && errno == EAGAIN && took >= 150 &&
                                                                           took < 2000);
    close(s);
    close(c);

    /* Nobody at a port. */
    c = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in nobody = v4("127.0.0.1", 9);
    check("a connection nobody answers is ECONNREFUSED",
          connect(c, (struct sockaddr *)&nobody, sizeof nobody) == -1 && errno == ECONNREFUSED);
    close(c);
    close(l);
}

static void on_alarm(int sig)
{
    (void)sig;
}

/* A blocking accept is a wait like any other: a signal with a handler that
   did not ask to be restarted ends it. */
static void interrupted(void)
{
    int l = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in at = v4("127.0.0.1", 0);
    bind(l, (struct sockaddr *)&at, sizeof at);
    listen(l, 1);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_alarm;
    sigaction(SIGALRM, &sa, NULL);
    struct itimerval it = {{0, 0}, {0, 200000}};
    setitimer(ITIMER_REAL, &it, NULL);
    errno = 0;
    int s = accept(l, NULL, NULL);
    check("a signal ends a blocking accept, with EINTR", s == -1 && errno == EINTR);
    signal(SIGALRM, SIG_DFL);
    close(l);
}

static void datagrams(void)
{
    int a = socket(AF_INET, SOCK_DGRAM, 0);
    int b = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
    struct sockaddr_in aa = v4("127.0.0.1", 0), ba = v4("127.0.0.1", 0);
    bind(a, (struct sockaddr *)&aa, sizeof aa);
    bind(b, (struct sockaddr *)&ba, sizeof ba);
    socklen_t len = sizeof aa;
    getsockname(a, (struct sockaddr *)&aa, &len);
    len = sizeof ba;
    getsockname(b, (struct sockaddr *)&ba, &len);
    check("two datagram sockets, on ports of their own", aa.sin_port && ba.sin_port && aa.sin_port != ba.sin_port);
    char buf[64];
    check("nothing to receive is EAGAIN", recv(b, buf, sizeof buf, 0) == -1 && errno == EAGAIN);
    check("sendto", sendto(a, "ping", 4, 0, (struct sockaddr *)&ba, sizeof ba) == 4);
    struct sockaddr_in from;
    len = sizeof from;
    check("and recvfrom, with who sent it", ready(b, POLLIN, 2000) &&
                                                recvfrom(b, buf, sizeof buf, 0, (struct sockaddr *)&from, &len) == 4 &&
                                                from.sin_port == aa.sin_port && memcmp(buf, "ping", 4) == 0);
    check("a datagram socket connected", connect(b, (struct sockaddr *)&aa, sizeof aa) == 0);
    check("sends by send", send(b, "pong", 4, 0) == 4);
    check("and the other receives it", ready(a, POLLIN, 2000) && recv(a, buf, sizeof buf, 0) == 4);
    sendto(a, "0123456789", 10, 0, (struct sockaddr *)&ba, sizeof ba);
    ready(b, POLLIN, 2000);
    struct iovec v = {buf, 4};
    struct msghdr m;
    memset(&m, 0, sizeof m);
    m.msg_iov = &v;
    m.msg_iovlen = 1;
    long n = recvmsg(b, &m, MSG_TRUNC);
    check("cut to the room there is, with MSG_TRUNC saying how long it was", n == 10 && (m.msg_flags & MSG_TRUNC));
    char one[3], two[4];
    struct iovec parts[2] = {{"abc", 3}, {"defg", 4}};
    memset(&m, 0, sizeof m);
    m.msg_iov = parts;
    m.msg_iovlen = 2;
    check("sendmsg in two pieces is one datagram", sendmsg(a, &(struct msghdr){.msg_name = &ba, .msg_namelen = sizeof ba,
                                                                           .msg_iov = parts, .msg_iovlen = 2},
                                                           0) == 7);
    struct iovec into[2] = {{one, 3}, {two, 4}};
    memset(&m, 0, sizeof m);
    m.msg_iov = into;
    m.msg_iovlen = 2;
    check("received in two pieces", ready(b, POLLIN, 2000) && recvmsg(b, &m, 0) == 7 && memcmp(one, "abc", 3) == 0 &&
                                        memcmp(two, "defg", 4) == 0);
    close(a);
    close(b);
}

static void ipv6(void)
{
    int l = socket(AF_INET6, SOCK_STREAM, 0);
    check("a stream socket of IPv6's family", l >= 0);
    struct sockaddr_in6 at = v6("::", 0);
    bind(l, (struct sockaddr *)&at, sizeof at);
    socklen_t len = sizeof at;
    getsockname(l, (struct sockaddr *)&at, &len);
    listen(l, 4);
    int c = socket(AF_INET6, SOCK_STREAM, 0);
    struct sockaddr_in6 to = v6("::1", ntohs(at.sin6_port));
    check("connected over ::1", connect(c, (struct sockaddr *)&to, sizeof to) == 0);
    struct sockaddr_in6 from;
    len = sizeof from;
    int s = accept(l, (struct sockaddr *)&from, &len);
    check("accepted from ::1", s >= 0 && from.sin6_family == AF_INET6 && IN6_IS_ADDR_LOOPBACK(&from.sin6_addr));
    close(s);
    close(c);
    c = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in to4 = v4("127.0.0.1", ntohs(at.sin6_port));
    connect(c, (struct sockaddr *)&to4, sizeof to4);
    len = sizeof from;
    s = accept(l, (struct sockaddr *)&from, &len);
    check("and IPv4 to it is ::ffff:127.0.0.1", s >= 0 && IN6_IS_ADDR_V4MAPPED(&from.sin6_addr) &&
                                                    from.sin6_addr.s6_addr[12] == 127 && from.sin6_addr.s6_addr[15] == 1);
    close(s);
    close(c);
    close(l);
    int only = socket(AF_INET6, SOCK_STREAM, 0);
    int yes = 1, said = 0;
    socklen_t slen = sizeof said;
    check("IPV6_V6ONLY is taken, and said", setsockopt(only, IPPROTO_IPV6, IPV6_V6ONLY, &yes, sizeof yes) == 0 &&
                                                 getsockopt(only, IPPROTO_IPV6, IPV6_V6ONLY, &said, &slen) == 0 &&
                                                 said == 1);
    close(only);
    struct sockaddr_in6 wrong = v6("::1", 9);
    int four = socket(AF_INET, SOCK_DGRAM, 0);
    check("an IPv6 address to a socket of IPv4's is EAFNOSUPPORT",
          connect(four, (struct sockaddr *)&wrong, sizeof wrong) == -1 && errno == EAFNOSUPPORT);
    close(four);
    /* The other way about is IPv4 in IPv6's clothes, as Linux has it. */
    int a = socket(AF_INET, SOCK_DGRAM, 0), six = socket(AF_INET6, SOCK_DGRAM, 0);
    struct sockaddr_in aa = v4("127.0.0.1", 0);
    bind(a, (struct sockaddr *)&aa, sizeof aa);
    socklen_t alen = sizeof aa;
    getsockname(a, (struct sockaddr *)&aa, &alen);
    char buf[8];
    check("an IPv4 address to a datagram socket of IPv6's reaches it",
          sendto(six, "v4", 2, 0, (struct sockaddr *)&aa, sizeof aa) == 2 && ready(a, POLLIN, 2000) &&
              recv(a, buf, sizeof buf, 0) == 2);
    close(a);
    close(six);
}

static void names(void)
{
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_socktype = SOCK_STREAM;
    int r = getaddrinfo("localhost", "80", &hints, &res);
    int four = 0, six = 0;
    for (struct addrinfo *p = res; r == 0 && p; p = p->ai_next) {
        if (p->ai_family == AF_INET)
            four |= ((struct sockaddr_in *)p->ai_addr)->sin_addr.s_addr == htonl(INADDR_LOOPBACK) &&
                    ((struct sockaddr_in *)p->ai_addr)->sin_port == htons(80);
        if (p->ai_family == AF_INET6)
            six |= IN6_IS_ADDR_LOOPBACK(&((struct sockaddr_in6 *)p->ai_addr)->sin6_addr);
    }
    check("getaddrinfo says localhost is 127.0.0.1 and ::1", r == 0 && four && six);
    if (res)
        freeaddrinfo(res);
    res = NULL;
    hints.ai_flags = AI_NUMERICHOST;
    r = getaddrinfo("::1", NULL, &hints, &res);
    check("and that ::1 is itself", r == 0 && res && res->ai_family == AF_INET6);
    if (res)
        freeaddrinfo(res);
    /* A name only a DNS server knows, asked of the machine's resolver at
       127.0.0.1, which /etc/resolv.conf, by saying nothing, leaves musl to
       ask: quark.localhost is the machine (RFC 6761), as the host's
       resolver says, which QEMU's DNS asks. */
    res = NULL;
    hints.ai_flags = 0;
    r = getaddrinfo("quark.localhost", "7", &hints, &res);
    four = six = 0;
    for (struct addrinfo *p = res; r == 0 && p; p = p->ai_next) {
        if (p->ai_family == AF_INET)
            four |= ((struct sockaddr_in *)p->ai_addr)->sin_addr.s_addr == htonl(INADDR_LOOPBACK);
        if (p->ai_family == AF_INET6)
            six |= IN6_IS_ADDR_LOOPBACK(&((struct sockaddr_in6 *)p->ai_addr)->sin6_addr);
    }
    check("and through the machine's resolver, that quark.localhost is both", r == 0 && four && six);
    if (res)
        freeaddrinfo(res);
}

int main(void)
{
    printf("inettest:\n");
    stream4();
    interrupted();
    datagrams();
    ipv6();
    names();
    printf("inettest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
