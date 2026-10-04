/* Sockets of the network: IPv4's and IPv6's families.
 *
 * The network stack (`net`) serves each as a descriptor, and this file is
 * Linux's shapes for what it is asked (docs/net.md): a sockaddr into three
 * words and back, Linux's options into the stack's, a message's pieces into
 * one datagram. Every request made here is one the stack answers at once,
 * and where Linux's call waits, it waits here, in a poll of the one
 * descriptor: so a signal ends it as it ends any other wait — EINTR, or
 * made again for a handler that asked — and SO_RCVTIMEO and SO_SNDTIMEO are
 * how long it may. A read and a write of the descriptor come here too, for
 * the same reasons, and so that what went wrong, which the kernel cannot
 * carry back from a server, is said as Linux says it.
 *
 * What the C library is told to keep — the timeouts, SO_REUSEADDR — is kept
 * here, in this program's memory: a program given the socket by another, or
 * by exec, starts with none of it, where on Linux it is the socket's.
 */

#include <quark/syscall.h>

#include "abi.h"

#define NULL ((void *)0)

/* Linux's names for what this file speaks. */
#define AF_UNSPEC 0
#define AF_INET   2
#define AF_INET6  10
#define SOCK_STREAM 1
#define SOCK_DGRAM  2
#define LX_SOCK_NONBLOCK 04000
#define LX_SOCK_CLOEXEC  02000000

#define SOL_SOCKET    1
#define SO_REUSEADDR  2
#define SO_TYPE       3
#define SO_ERROR      4
#define SO_BROADCAST  6
#define SO_SNDBUF     7
#define SO_RCVBUF     8
#define SO_KEEPALIVE  9
#define SO_LINGER     13
#define SO_REUSEPORT  15
#define SO_PASSCRED   16
#define SO_RCVTIMEO   20
#define SO_SNDTIMEO   21
#define SO_ACCEPTCONN 30
#define SO_PROTOCOL   38
#define SO_DOMAIN     39
#define IPPROTO_IP    0
#define IP_TOS        1
#define IP_TTL        2
#define IPPROTO_TCP   6
#define TCP_NODELAY   1
#define TCP_MAXSEG    2
#define TCP_KEEPIDLE  4
#define TCP_KEEPINTVL 5
#define TCP_KEEPCNT   6
#define IPPROTO_UDP   17
#define IPPROTO_IPV6  41
#define IPV6_UNICAST_HOPS 16
#define IPV6_V6ONLY   26

#define MSG_PEEK     0x2
#define MSG_TRUNC    0x20
#define MSG_DONTWAIT 0x40
#define MSG_WAITALL  0x100
#define MSG_NOSIGNAL 0x4000

/* The stack's, as docs/net.md says them. */
#define TAG_SOCKET   20
#define OP_CREATE    0
#define OP_BIND      1
#define OP_LISTEN    2
#define OP_CONNECT   3
#define OP_ACCEPT    4
#define OP_SEND_TO   5
#define OP_RECV_FROM 6
#define OP_SHUTDOWN  7
#define OP_NAME      8
#define OP_OPTION    9
#define NOT_WAITING  (1UL << 8)
#define ONLY_LOOKING (2UL << 8)
#define OPT_ERROR     1
#define OPT_KEEPALIVE 2
#define OPT_NODELAY   3
#define OPT_LISTENING 4
#define OPT_TYPE      5
#define OPT_V6ONLY    6
#define OPT_RCVBUF    7
#define OPT_SNDBUF    8
#define OPT_PENDING   10

/* The most one datagram carries over this machine's own addresses. */
#define DATAGRAM_MAX 65507

struct lx_iovec {
    void *iov_base;
    unsigned long iov_len;
};

struct lx_msghdr {
    void *msg_name;
    unsigned int msg_namelen;
    struct lx_iovec *msg_iov;
    unsigned long msg_iovlen;
    void *msg_control;
    unsigned long msg_controllen;
    int msg_flags;
};

struct lx_sockaddr_in {
    unsigned short family;
    unsigned short port;
    unsigned char addr[4];
    unsigned char zero[8];
};

struct lx_sockaddr_in6 {
    unsigned short family;
    unsigned short port;
    unsigned int flowinfo;
    unsigned char addr[16];
    unsigned int scope;
};

/* What a sockaddr_in6 must be at least, as Linux measures one. */
#define SIN6_LEN 24

struct lx_timeval {
    long sec;
    long usec;
};

struct qw_pollfd {
    unsigned int fd;
    unsigned int events;
    unsigned int revents;
    unsigned int pad;
};

#define QW_READABLE 1
#define QW_WRITABLE 2

/* A socket, as the stack names it. */
struct sock {
    unsigned long server;
    unsigned long cookie;
};

/* How long a receive and a send may wait, in nanoseconds — nought for as
   long as it takes — and whether SO_REUSEADDR was said. */
static unsigned long rcv_timeout[MAX_FDS];
static unsigned long snd_timeout[MAX_FDS];
static unsigned char reuse[MAX_FDS];

/* Gathering a datagram from several pieces, or scattering one into them:
   one buffer, one thread at a time. */
static unsigned char gathered[DATAGRAM_MAX];
static int gathered_lock;

static unsigned long net_tid;

static void bytes_copy(void *to, const void *from, unsigned long n) {
    unsigned char *t = to;
    const unsigned char *f = from;
    while (n--) {
        *t++ = *f++;
    }
}

static void bytes_zero(void *p, unsigned long n) {
    unsigned char *b = p;
    while (n--) {
        *b++ = 0;
    }
}

static unsigned short swap16(unsigned short v) {
    return (unsigned short)((v >> 8) | (v << 8));
}

static unsigned long le64(const unsigned char *b) {
    unsigned long w = 0;
    for (int i = 7; i >= 0; i--) {
        w = w << 8 | b[i];
    }
    return w;
}

static void unle64(unsigned long w, unsigned char *b) {
    for (int i = 0; i < 8; i++) {
        b[i] = (unsigned char)(w >> (8 * i));
    }
}

unsigned long __quark_net(int again) {
    /* Looked up once; and again when asked, because a lookup is also what
       gives a program the right to call the stack, which a program given
       its socket by another may not have. */
    if (again || !__atomic_load_n(&net_tid, __ATOMIC_RELAXED)) {
        __atomic_store_n(&net_tid, (unsigned long)quark_lookup("net"), __ATOMIC_RELAXED);
    }
    return __atomic_load_n(&net_tid, __ATOMIC_RELAXED);
}

void __quark_inet_forget(long fd) {
    if (fd >= 0 && fd < MAX_FDS) {
        rcv_timeout[fd] = 0;
        snd_timeout[fd] = 0;
        reuse[fd] = 0;
    }
}

static int sock_of(long fd, struct sock *s) {
    unsigned long named[2];
    if (fd < 0 || __syscall2(SYS_FD_SERVED, (unsigned long)fd, (unsigned long)named) == QUARK_ERR) {
        return 0;
    }
    s->server = named[0];
    s->cookie = named[1];
    return 1;
}

/* Ask the stack about socket `s`, lending `buf` if there is one: 0, or the
   errno it answered with, negated. */
static long ask(const struct sock *s, unsigned long op, unsigned long a, unsigned long b, unsigned long c,
                unsigned long d, void *buf, unsigned long len, unsigned long access, struct quark_msg *reply) {
    struct quark_msg msg;
    msg.sender = 0;
    msg.tag = TAG_SOCKET;
    msg.data[0] = op;
    msg.data[1] = s->cookie;
    msg.data[2] = a;
    msg.data[3] = b;
    msg.data[4] = c;
    msg.data[5] = d;
    for (int tries = 0;; tries++) {
        int r = buf && len ? quark_call_lend(s->server, &msg, reply, buf, len, access)
                           : quark_call(s->server, &msg, reply);
        if (r == 0) {
            break;
        }
        if (tries || __quark_net(1) != s->server) {
            return -LX_ENETDOWN;
        }
    }
    return reply->tag == ~0UL ? -(long)reply->data[0] : 0;
}

/* Three words for the stack from a sockaddr of either family: 0, or a
   negative errno for one too short or of another family. The stack says
   whether the socket's family will take it. */
static long words_from(const void *addr, unsigned long len, unsigned long w[3]) {
    if (!addr) {
        return -LX_EFAULT;
    }
    if (len < sizeof(unsigned short)) {
        return -LX_EINVAL;
    }
    unsigned char b[16];
    bytes_zero(b, sizeof b);
    unsigned short family = *(const unsigned short *)addr;
    unsigned short port;
    if (family == AF_INET) {
        if (len < sizeof(struct lx_sockaddr_in)) {
            return -LX_EINVAL;
        }
        const struct lx_sockaddr_in *a = addr;
        port = swap16(a->port);
        bytes_copy(b, a->addr, 4);
    } else if (family == AF_INET6) {
        if (len < SIN6_LEN) {
            return -LX_EINVAL;
        }
        const struct lx_sockaddr_in6 *a = addr;
        port = swap16(a->port);
        bytes_copy(b, a->addr, 16);
    } else {
        return -LX_EAFNOSUPPORT;
    }
    w[0] = (unsigned long)family << 16 | port;
    w[1] = le64(b);
    w[2] = le64(b + 8);
    return 0;
}

/* A sockaddr from three of the stack's words, into `addr` with room for
   `*len` bytes; `*len` is then how long the whole of it is, as Linux says. */
static void sockaddr_to(unsigned long famport, unsigned long a0, unsigned long a1, void *addr, unsigned int *len) {
    if (!addr || !len) {
        return;
    }
    unsigned char b[16];
    unle64(a0, b);
    unle64(a1, b + 8);
    unsigned short port = swap16((unsigned short)famport);
    union {
        struct lx_sockaddr_in in;
        struct lx_sockaddr_in6 in6;
    } u;
    bytes_zero(&u, sizeof u);
    unsigned int whole;
    if (famport >> 16 == AF_INET6) {
        u.in6.family = AF_INET6;
        u.in6.port = port;
        bytes_copy(u.in6.addr, b, 16);
        whole = sizeof u.in6;
    } else {
        u.in.family = AF_INET;
        u.in.port = port;
        bytes_copy(u.in.addr, b, 4);
        whole = sizeof u.in;
    }
    bytes_copy(addr, &u, *len < whole ? *len : whole);
    *len = whole;
}

/* When a wait of `timeout` nanoseconds from now is over: nought for never. */
static unsigned long deadline_after(unsigned long timeout) {
    return timeout ? quark_now() + timeout : 0;
}

/* Wait for `fd` to be ready for `events`, until `deadline`: nought to try
   again, -EAGAIN when the time is up, -EINTR when a signal's handler ended
   the wait. A handler that asked to be restarted is a try again — unless
   there is a time limit, which Linux does not restart. */
static long wait_for(long fd, unsigned int events, unsigned long deadline) {
    struct qw_pollfd q = {(unsigned int)fd, events, 0, 0};
    unsigned long span = ~0UL;
    if (deadline) {
        unsigned long now = quark_now();
        if (now >= deadline) {
            return -LX_EAGAIN;
        }
        span = quark_span(deadline - now);
    }
    unsigned long n = __syscall5(SYS_POLL, (unsigned long)&q, 1, span, 0, 0);
    long cut = quark_cut_short(n, deadline == 0);
    if (cut < 0) {
        return cut;
    }
    if (cut > 0) {
        return 0;
    }
    if (n == QUARK_ERR) {
        return -LX_EBADF;
    }
    return n == 0 && deadline ? -LX_EAGAIN : 0;
}

/* Whether `fd` is ready for `events` now. */
static int ready_now(long fd, unsigned int events) {
    struct qw_pollfd q = {(unsigned int)fd, events, 0, 0};
    unsigned long n = __syscall5(SYS_POLL, (unsigned long)&q, 1, quark_span(0), 0, 0);
    return n != QUARK_ERR && n > 0 && n < QUARK_AGAIN && (q.revents & events);
}

static int waits(long fd, long flags) {
    return !(flags & MSG_DONTWAIT) && !__quark_fd_is_nonblock(fd);
}

long __quark_inet_socket(long domain, long type, long protocol) {
    /* No network at all is as if the family were not there. */
    unsigned long net = __quark_net(0);
    if (!net) {
        return -LX_EAFNOSUPPORT;
    }
    struct quark_msg msg, reply;
    bytes_zero(&msg, sizeof msg);
    msg.tag = TAG_SOCKET;
    msg.data[0] = OP_CREATE;
    msg.data[1] = (unsigned long)domain;
    msg.data[2] = (unsigned long)type & 0xF;
    msg.data[3] = (unsigned long)protocol;
    if (quark_call(net, &msg, &reply) != 0) {
        /* The stack may have been started again since it was looked up,
           and the old one is gone: who it is, once more. */
        net = __quark_net(1);
        if (!net || quark_call(net, &msg, &reply) != 0) {
            return -LX_ENETDOWN;
        }
    }
    if (reply.tag == ~0UL) {
        return -(long)reply.data[0];
    }
    long fd = (long)reply.data[0];
    __quark_fd_forget(fd);
    if (type & LX_SOCK_NONBLOCK) {
        __quark_fd_set_nonblock(fd, 1);
    }
    if (type & LX_SOCK_CLOEXEC) {
        __syscall3(SYS_FD_FLAGS, (unsigned long)fd, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
    }
    return fd;
}

long __quark_inet_bind(long fd, const void *addr, unsigned long len) {
    struct sock s;
    unsigned long w[3];
    struct quark_msg reply;
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    long bad = words_from(addr, len, w);
    return bad ? bad : ask(&s, OP_BIND, w[0], w[1], w[2], 0, NULL, 0, 0, &reply);
}

long __quark_inet_listen(long fd, long backlog) {
    struct sock s;
    struct quark_msg reply;
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    return ask(&s, OP_LISTEN, backlog < 0 ? 0 : (unsigned long)backlog, 0, 0, 0, NULL, 0, 0, &reply);
}

long __quark_inet_connect(long fd, const void *addr, unsigned long len) {
    struct sock s;
    struct quark_msg reply;
    unsigned long w[3] = {0, 0, 0};
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    /* AF_UNSPEC: a datagram socket's correspondent forgotten. */
    if (!(addr && len >= sizeof(unsigned short) && *(const unsigned short *)addr == AF_UNSPEC)) {
        long bad = words_from(addr, len, w);
        if (bad) {
            return bad;
        }
    }
    long r = ask(&s, OP_CONNECT | NOT_WAITING, w[0], w[1], w[2], 0, NULL, 0, 0, &reply);
    if (r != -LX_EINPROGRESS || __quark_fd_is_nonblock(fd)) {
        return r;
    }
    /* Linux's connect waits until it knows; this one in a poll, which a
       signal ends. Ended so, the connection goes on being made, as there. */
    unsigned long deadline = deadline_after(fd < MAX_FDS ? snd_timeout[fd] : 0);
    for (;;) {
        long w8 = wait_for(fd, QW_WRITABLE, deadline);
        if (w8 == -LX_EAGAIN) {
            return -LX_EINPROGRESS;
        }
        if (w8 < 0) {
            return w8;
        }
        r = ask(&s, OP_OPTION, OPT_ERROR, 0, 0, 0, NULL, 0, 0, &reply);
        if (r < 0) {
            return r;
        }
        if (reply.data[0]) {
            return -(long)reply.data[0];
        }
        if (ready_now(fd, QW_WRITABLE)) {
            return 0;
        }
    }
}

long __quark_inet_accept(long fd, void *addr, unsigned int *len, long flags) {
    struct sock s;
    struct quark_msg reply;
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    unsigned long deadline = deadline_after(fd < MAX_FDS ? rcv_timeout[fd] : 0);
    for (;;) {
        long r = ask(&s, OP_ACCEPT | NOT_WAITING, 0, 0, 0, 0, NULL, 0, 0, &reply);
        if (r == 0) {
            break;
        }
        if (r != -LX_EAGAIN || !waits(fd, 0)) {
            return r;
        }
        long w8 = wait_for(fd, QW_READABLE, deadline);
        if (w8 < 0) {
            return w8;
        }
    }
    long nfd = (long)reply.data[0];
    __quark_fd_forget(nfd);
    if (flags & LX_SOCK_NONBLOCK) {
        __quark_fd_set_nonblock(nfd, 1);
    }
    if (flags & LX_SOCK_CLOEXEC) {
        __syscall3(SYS_FD_FLAGS, (unsigned long)nfd, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
    }
    sockaddr_to(reply.data[1], reply.data[2], reply.data[3], addr, len);
    return nfd;
}

long __quark_inet_name(long fd, void *addr, unsigned int *len, int peer) {
    struct sock s;
    struct quark_msg reply;
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    if (!addr || !len) {
        return -LX_EFAULT;
    }
    long r = ask(&s, OP_NAME, peer ? 1 : 0, 0, 0, 0, NULL, 0, 0, &reply);
    if (r == 0) {
        sockaddr_to(reply.data[0], reply.data[1], reply.data[2], addr, len);
    }
    return r;
}

long __quark_inet_shutdown(long fd, long how) {
    struct sock s;
    struct quark_msg reply;
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    return ask(&s, OP_SHUTDOWN, (unsigned long)how, 0, 0, 0, NULL, 0, 0, &reply);
}

/* `len` bytes to `to` — nought for the one it is connected to — waiting for
   room as Linux would: a stream sends the whole of it, unless it may not
   wait or a signal comes, and then says how much went. */
static long send_bytes(long fd, const struct sock *s, const void *buf, unsigned long len, const unsigned long to[3],
                       long flags) {
    unsigned long deadline = deadline_after(fd < MAX_FDS ? snd_timeout[fd] : 0);
    unsigned long sent = 0;
    for (;;) {
        struct quark_msg reply;
        unsigned long left = len - sent;
        long r = ask(s, OP_SEND_TO | NOT_WAITING, to[0], to[1], to[2], left, (char *)buf + sent, left,
                     QUARK_LEND_READ, &reply);
        if (r == 0) {
            sent += reply.data[0];
            if (sent >= len || !waits(fd, flags)) {
                return (long)sent;
            }
            continue;
        }
        if (r == -LX_EPIPE && !(flags & MSG_NOSIGNAL)) {
            __quark_sig_pipe();
        }
        if (r != -LX_EAGAIN || !waits(fd, flags)) {
            return sent ? (long)sent : r;
        }
        long w8 = wait_for(fd, QW_WRITABLE, deadline);
        if (w8 < 0) {
            return sent ? (long)sent : w8;
        }
    }
}

/* Bytes or a datagram into `buf`, waiting as Linux would: how much; and
   where it came from, and how long a datagram was, into `from`. */
static long recv_bytes(long fd, const struct sock *s, void *buf, unsigned long len, long flags, unsigned long from[4]) {
    unsigned long deadline = deadline_after(fd < MAX_FDS ? rcv_timeout[fd] : 0);
    unsigned long op = OP_RECV_FROM | NOT_WAITING | ((flags & MSG_PEEK) ? ONLY_LOOKING : 0);
    for (;;) {
        struct quark_msg reply;
        long r = ask(s, op, len, 0, 0, 0, buf, len, QUARK_LEND_WRITE, &reply);
        if (r == 0) {
            if (from) {
                from[0] = reply.data[1];
                from[1] = reply.data[2];
                from[2] = reply.data[3];
                from[3] = reply.data[4];
            }
            return (long)reply.data[0];
        }
        if (r != -LX_EAGAIN || !waits(fd, flags)) {
            return r;
        }
        long w8 = wait_for(fd, QW_READABLE, deadline);
        if (w8 < 0) {
            return w8;
        }
    }
}

/* Whether socket `s` is a stream, as the stack says. */
static int is_stream(const struct sock *s) {
    struct quark_msg reply;
    return ask(s, OP_OPTION, OPT_TYPE, 0, 0, 0, NULL, 0, 0, &reply) == 0 && reply.data[0] == SOCK_STREAM;
}

long __quark_inet_sendmsg(long fd, const void *msg, long flags) {
    const struct lx_msghdr *m = msg;
    struct sock s;
    unsigned long to[3] = {0, 0, 0};
    if (!m) {
        return -LX_EFAULT;
    }
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    if (m->msg_name && m->msg_namelen) {
        long bad = words_from(m->msg_name, m->msg_namelen, to);
        if (bad) {
            return bad;
        }
    }
    unsigned long total = 0, pieces = 0;
    const struct lx_iovec *only = NULL;
    for (unsigned long i = 0; i < m->msg_iovlen; i++) {
        if (m->msg_iov[i].iov_len) {
            total += m->msg_iov[i].iov_len;
            pieces++;
            only = &m->msg_iov[i];
        }
    }
    if (pieces <= 1) {
        return send_bytes(fd, &s, only ? only->iov_base : NULL, only ? only->iov_len : 0, to, flags);
    }
    if (is_stream(&s)) {
        /* A stream has no edges: piece after piece, until one is cut short. */
        long sent = 0;
        for (unsigned long i = 0; i < m->msg_iovlen; i++) {
            const struct lx_iovec *v = &m->msg_iov[i];
            if (!v->iov_len) {
                continue;
            }
            long n = send_bytes(fd, &s, v->iov_base, v->iov_len, to, flags);
            if (n < 0) {
                return sent ? sent : n;
            }
            sent += n;
            if ((unsigned long)n < v->iov_len) {
                break;
            }
        }
        return sent;
    }
    /* A datagram is one whatever it was in: gathered first. */
    if (total > DATAGRAM_MAX) {
        return -LX_EMSGSIZE;
    }
    __quark_lock(&gathered_lock);
    unsigned long at = 0;
    for (unsigned long i = 0; i < m->msg_iovlen; i++) {
        bytes_copy(gathered + at, m->msg_iov[i].iov_base, m->msg_iov[i].iov_len);
        at += m->msg_iov[i].iov_len;
    }
    long n = send_bytes(fd, &s, gathered, total, to, flags);
    __quark_unlock(&gathered_lock);
    return n;
}

long __quark_inet_recvmsg(long fd, void *msg, long flags) {
    struct lx_msghdr *m = msg;
    struct sock s;
    unsigned long from[4] = {0, 0, 0, 0};
    if (!m) {
        return -LX_EFAULT;
    }
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    m->msg_flags = 0;
    m->msg_controllen = 0;
    unsigned long room = 0, pieces = 0;
    struct lx_iovec *only = NULL;
    for (unsigned long i = 0; i < m->msg_iovlen; i++) {
        if (m->msg_iov[i].iov_len) {
            room += m->msg_iov[i].iov_len;
            pieces++;
            only = &m->msg_iov[i];
        }
    }
    int stream = is_stream(&s);
    long n;
    if (stream) {
        /* Into each piece in turn, the first as Linux would wait for, the
           rest with what is there already — or, with MSG_WAITALL, until
           every piece is full or the stream has ended. */
        n = 0;
        for (unsigned long i = 0; i < m->msg_iovlen; i++) {
            struct lx_iovec *v = &m->msg_iov[i];
            unsigned long filled = 0;
            while (filled < v->iov_len) {
                long f = (n == 0 && filled == 0) || (flags & MSG_WAITALL) ? flags : flags | MSG_DONTWAIT;
                long got = recv_bytes(fd, &s, (char *)v->iov_base + filled, v->iov_len - filled, f, NULL);
                if (got <= 0) {
                    if (n || filled) {
                        return n + (long)filled;
                    }
                    return got;
                }
                filled += (unsigned long)got;
                if (!(flags & MSG_WAITALL)) {
                    break;
                }
            }
            n += (long)filled;
            if (filled < v->iov_len) {
                break;
            }
        }
        return n;
    }
    unsigned long whole;
    if (pieces <= 1) {
        n = recv_bytes(fd, &s, only ? only->iov_base : NULL, only ? only->iov_len : 0, flags, from);
        whole = from[3];
    } else {
        /* One datagram, scattered over the pieces. */
        __quark_lock(&gathered_lock);
        n = recv_bytes(fd, &s, gathered, room < DATAGRAM_MAX ? room : DATAGRAM_MAX, flags, from);
        if (n > 0) {
            unsigned long at = 0;
            for (unsigned long i = 0; i < m->msg_iovlen && at < (unsigned long)n; i++) {
                unsigned long k = m->msg_iov[i].iov_len;
                if (k > (unsigned long)n - at) {
                    k = (unsigned long)n - at;
                }
                bytes_copy(m->msg_iov[i].iov_base, gathered + at, k);
                at += k;
            }
        }
        __quark_unlock(&gathered_lock);
        whole = from[3];
    }
    if (n < 0) {
        return n;
    }
    if (whole > (unsigned long)n) {
        m->msg_flags |= MSG_TRUNC;
        if (flags & MSG_TRUNC) {
            n = (long)whole;
        }
    }
    if (m->msg_name) {
        sockaddr_to(from[0], from[1], from[2], m->msg_name, &m->msg_namelen);
    }
    return n;
}

long __quark_inet_sendto(long fd, const void *buf, unsigned long len, long flags, const void *addr,
                         unsigned long alen) {
    struct lx_iovec v = {(void *)buf, len};
    struct lx_msghdr m = {(void *)addr, (unsigned int)alen, &v, 1, NULL, 0, 0};
    return __quark_inet_sendmsg(fd, &m, flags);
}

long __quark_inet_recvfrom(long fd, void *buf, unsigned long len, long flags, void *addr, unsigned int *alen) {
    struct lx_iovec v = {buf, len};
    struct lx_msghdr m = {addr, alen ? *alen : 0, &v, 1, NULL, 0, 0};
    long n = __quark_inet_recvmsg(fd, &m, flags);
    if (n >= 0 && addr && alen) {
        *alen = m.msg_namelen;
    }
    return n;
}

long __quark_inet_read(long fd, void *buf, unsigned long len) {
    return len ? __quark_inet_recvfrom(fd, buf, len, 0, NULL, NULL) : 0;
}

long __quark_inet_write(long fd, const void *buf, unsigned long len) {
    return __quark_inet_sendto(fd, buf, len, 0, NULL, 0);
}

long __quark_inet_pending(long fd, int *count) {
    struct sock s;
    struct quark_msg reply;
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    long r = ask(&s, OP_OPTION, OPT_PENDING, 0, 0, 0, NULL, 0, 0, &reply);
    if (r == 0 && count) {
        *count = (int)reply.data[0];
    }
    return r;
}

/* An int option's value, as Linux hands one back. */
static long int_answer(void *val, unsigned int *len, long v) {
    if (*len < sizeof(int)) {
        return -LX_EINVAL;
    }
    *(int *)val = (int)v;
    *len = sizeof(int);
    return 0;
}

static long time_answer(void *val, unsigned int *len, unsigned long ns) {
    struct lx_timeval tv = {(long)(ns / 1000000000UL), (long)(ns % 1000000000UL / 1000)};
    unsigned int n = *len < sizeof tv ? *len : (unsigned int)sizeof tv;
    bytes_copy(val, &tv, n);
    *len = n;
    return 0;
}

long __quark_inet_getsockopt(long fd, long level, long name, void *val, unsigned int *len) {
    struct sock s;
    struct quark_msg reply;
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    if (!val || !len) {
        return -LX_EFAULT;
    }
    unsigned long opt = 0;
    switch (level) {
    case SOL_SOCKET:
        switch (name) {
        case SO_ERROR: opt = OPT_ERROR; break;
        case SO_TYPE: opt = OPT_TYPE; break;
        case SO_ACCEPTCONN: opt = OPT_LISTENING; break;
        case SO_KEEPALIVE: opt = OPT_KEEPALIVE; break;
        case SO_SNDBUF: opt = OPT_SNDBUF; break;
        case SO_RCVBUF: opt = OPT_RCVBUF; break;
        case SO_PROTOCOL:
            return int_answer(val, len, is_stream(&s) ? IPPROTO_TCP : IPPROTO_UDP);
        case SO_DOMAIN: {
            long r = ask(&s, OP_NAME, 0, 0, 0, 0, NULL, 0, 0, &reply);
            return r ? r : int_answer(val, len, (long)(reply.data[0] >> 16));
        }
        case SO_REUSEADDR:
            return int_answer(val, len, fd < MAX_FDS ? reuse[fd] : 0);
        case SO_REUSEPORT:
        case SO_BROADCAST:
        case SO_PASSCRED:
            return int_answer(val, len, 0);
        case SO_RCVTIMEO:
            return time_answer(val, len, fd < MAX_FDS ? rcv_timeout[fd] : 0);
        case SO_SNDTIMEO:
            return time_answer(val, len, fd < MAX_FDS ? snd_timeout[fd] : 0);
        case SO_LINGER: {
            /* Off: a close says goodbye, and does not wait for it. */
            int off[2] = {0, 0};
            unsigned int n = *len < sizeof off ? *len : (unsigned int)sizeof off;
            bytes_copy(val, off, n);
            *len = n;
            return 0;
        }
        default:
            return -LX_ENOPROTOOPT;
        }
        break;
    case IPPROTO_TCP:
        switch (name) {
        case TCP_NODELAY: opt = OPT_NODELAY; break;
        case TCP_MAXSEG: return int_answer(val, len, 1460);
        case TCP_KEEPIDLE:
        case TCP_KEEPINTVL: return int_answer(val, len, 75);
        case TCP_KEEPCNT: return int_answer(val, len, 9);
        default: return -LX_ENOPROTOOPT;
        }
        break;
    case IPPROTO_IP:
        switch (name) {
        case IP_TOS: return int_answer(val, len, 0);
        case IP_TTL: return int_answer(val, len, 64);
        default: return -LX_ENOPROTOOPT;
        }
    case IPPROTO_IPV6:
        switch (name) {
        case IPV6_V6ONLY: opt = OPT_V6ONLY; break;
        case IPV6_UNICAST_HOPS: return int_answer(val, len, 64);
        default: return -LX_ENOPROTOOPT;
        }
        break;
    default:
        return -LX_ENOPROTOOPT;
    }
    long r = ask(&s, OP_OPTION, opt, 0, 0, 0, NULL, 0, 0, &reply);
    return r ? r : int_answer(val, len, (long)reply.data[0]);
}

/* A timeval as nanoseconds, or a negative errno. */
static long timeval_ns(const void *val, unsigned long len, unsigned long *ns) {
    if (len < sizeof(struct lx_timeval)) {
        return -LX_EINVAL;
    }
    const struct lx_timeval *tv = val;
    if (tv->sec < 0 || tv->usec < 0 || tv->usec >= 1000000) {
        return -LX_EDOM;
    }
    *ns = (unsigned long)tv->sec * 1000000000UL + (unsigned long)tv->usec * 1000UL;
    return 0;
}

long __quark_inet_setsockopt(long fd, long level, long name, const void *val, unsigned long len) {
    struct sock s;
    struct quark_msg reply;
    if (!sock_of(fd, &s)) {
        return -LX_EBADF;
    }
    if (!val) {
        return -LX_EFAULT;
    }
    int v = len >= sizeof(int) ? *(const int *)val : 0;
    unsigned long opt = 0;
    switch (level) {
    case SOL_SOCKET:
        switch (name) {
        case SO_KEEPALIVE: opt = OPT_KEEPALIVE; break;
        case SO_REUSEADDR:
            if (fd < MAX_FDS) {
                reuse[fd] = v != 0;
            }
            return 0;
        case SO_RCVTIMEO:
        case SO_SNDTIMEO: {
            unsigned long ns;
            long bad = timeval_ns(val, len, &ns);
            if (bad) {
                return bad;
            }
            if (fd < MAX_FDS) {
                (name == SO_RCVTIMEO ? rcv_timeout : snd_timeout)[fd] = ns;
            }
            return 0;
        }
        /* Taken and not kept: the buffers are what they are, a port is
           free again as soon as its socket has gone, and a close says
           goodbye without waiting for it. */
        case SO_REUSEPORT:
        case SO_BROADCAST:
        case SO_SNDBUF:
        case SO_RCVBUF:
        case SO_LINGER:
        case SO_PASSCRED:
            return 0;
        default:
            return -LX_ENOPROTOOPT;
        }
        break;
    case IPPROTO_TCP:
        switch (name) {
        case TCP_NODELAY: opt = OPT_NODELAY; break;
        case TCP_MAXSEG:
        case TCP_KEEPIDLE:
        case TCP_KEEPINTVL:
        case TCP_KEEPCNT:
            return 0;
        default:
            return -LX_ENOPROTOOPT;
        }
        break;
    case IPPROTO_IP:
        switch (name) {
        case IP_TOS:
        case IP_TTL:
            return 0;
        default:
            return -LX_ENOPROTOOPT;
        }
    case IPPROTO_IPV6:
        switch (name) {
        case IPV6_V6ONLY: opt = OPT_V6ONLY; break;
        case IPV6_UNICAST_HOPS: return 0;
        default: return -LX_ENOPROTOOPT;
        }
        break;
    default:
        return -LX_ENOPROTOOPT;
    }
    if (len < sizeof(int)) {
        return -LX_EINVAL;
    }
    return ask(&s, OP_OPTION, opt, (unsigned long)(v != 0), 1, 0, NULL, 0, 0, &reply);
}
