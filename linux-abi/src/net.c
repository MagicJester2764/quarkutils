/* Linux's shapes for a stream, a descriptor in flight, and waiting.
 *
 * Quark's calls are the same operations with the arguments in a different
 * order and none of the packaging, so most of this file is unwrapping: a
 * `struct msghdr` into a pointer and a length, a `cmsghdr` into one descriptor
 * number, a `struct pollfd` array into Quark's.
 *
 * A message carries as many descriptors as the kernel's stream takes at once
 * (32), from every SCM_RIGHTS it has, and a receive takes as many as the
 * caller has room for. Credentials (SCM_CREDENTIALS) go to a receiver that
 * asked for them (SO_PASSCRED): who is at the other end, which is what the
 * kernel knows of a stream (SO_PEERCRED). And a socket of the local family
 * can have a name: the file server gives it one, and connects one to it.
 */

#include <quark/syscall.h>
#include <quark/vfs.h>

#include "abi.h"

typedef long ssize_t;
typedef unsigned long size_t;

#define NULL ((void *)0)

struct iovec {
    void *iov_base;
    unsigned long iov_len;
};

struct msghdr {
    void *msg_name;
    unsigned int msg_namelen;
    struct iovec *msg_iov;
    unsigned long msg_iovlen;
    void *msg_control;
    unsigned long msg_controllen;
    int msg_flags;
};

struct cmsghdr {
    unsigned long cmsg_len;
    int cmsg_level;
    int cmsg_type;
};

#define SOL_SOCKET  1
#define SCM_RIGHTS  1
#define SCM_CREDENTIALS 2
#define MSG_CTRUNC  0x8

/* The most descriptors one message carries: what the kernel's stream takes
   in one send (QUARK_FD_MANY). */
#define FDS_AT_ONCE 32

/* The message flags that change what a call does here: do not wait, and do
 * not raise SIGPIPE at a stream nobody is reading. */
#define MSG_DONTWAIT 0x40
#define MSG_NOSIGNAL 0x4000


#define AF_UNIX      1
#define SOCK_STREAM  1

/* Linux's poll bits, which are not Quark's. */
#define LX_POLLIN   0x001
#define LX_POLLOUT  0x004
#define LX_POLLERR  0x008
#define LX_POLLHUP  0x010
#define LX_POLLNVAL 0x020

struct lx_pollfd {
    int fd;
    short events;
    short revents;
};

/* Quark's, from the ABI: 1 readable, 2 writable, 4 hangup, 8 invalid. */
struct qw_pollfd {
    unsigned int fd;
    unsigned int events;
    unsigned int revents;
    unsigned int pad;
};

#define QW_READABLE 1
#define QW_WRITABLE 2
#define QW_HANGUP   4
#define QW_INVALID  8

#define MAX_POLL 32

long __quark_memfd(const char *name, long flags) {
    (void)name;  /* Linux keeps it for /proc; there is no /proc here. */
    (void)flags;
    /* One page, because the size comes from the ftruncate that follows -- and
       on Linux it must, since memfd_create takes no size at all. */
    unsigned long fd = __syscall1(SYS_MEMFD_CREATE, 1);
    if (fd == QUARK_ERR) {
        return -LX_ENOMEM;
    }
    return (long)fd;
}

/* Set the size of memory named by a descriptor.
 *
 * Only memory: a file's size is the VFS's business and nothing here can change
 * it, so a file earns EINVAL rather than a lie about having been resized. The
 * memory case is the one that matters, because `memfd_create` then `ftruncate`
 * then `mmap` is how every Wayland client makes its buffer pool. */
long __quark_ftruncate(long fd, long length) {
    if (length < 0) {
        return -LX_EINVAL;
    }
    unsigned long r = __syscall2(SYS_MEMFD_TRUNCATE, (unsigned long)fd,
                                 (unsigned long)length);
    return r == QUARK_ERR ? -LX_EINVAL : 0;
}

/* An ordinary pipe.
 *
 * Quark makes the pipe and then names its ends separately, so this is three
 * calls where Linux has one. Both ends go into this task's own table, which
 * needs no authority: `pipe` is something every C library does on its own
 * behalf and confers nothing on anybody else.
 *
 * The clipboard is what needed this. A client pasting creates a pipe, hands
 * the write end to the compositor, and reads the read end until end of file --
 * which is the whole reason the compositor never sees the data. */
long __quark_pipe(int *fds, long flags) {
    /* O_CLOEXEC survives an exec here rather than closing, because a Quark
       descriptor table is kept across `SYS_EXEC_SPACE` and nothing marks
       entries; a program that depends on it closes what it does not want.
       O_NONBLOCK is honoured: both ends are recorded as non-blocking, and the
       read and write paths use the calls that answer EAGAIN. */
    if (!fds) {
        return -LX_EFAULT;
    }
    unsigned long handle = __syscall0(SYS_PIPE_CREATE);
    if (handle == QUARK_ERR) {
        return -LX_EMFILE;
    }
    unsigned long me = __syscall0(SYS_GETPID);
    unsigned long r = __syscall4(SYS_PIPE_FD_SET, me, QUARK_ANY_FD, handle, 0);
    if (r == QUARK_ERR) {
        return -LX_EMFILE;
    }
    unsigned long w = __syscall4(SYS_PIPE_FD_SET, me, QUARK_ANY_FD, handle, 1);
    if (w == QUARK_ERR) {
        /* Give the read end back rather than leaving a half-made pipe in the
           table, which the caller has no way to find or close. */
        __syscall1(SYS_FD_CLOSE, r);
        return -LX_EMFILE;
    }
    fds[0] = (int)r;
    fds[1] = (int)w;
    if (flags & LX_O_NONBLOCK) {
        __quark_fd_set_nonblock((long)r, 1);
        __quark_fd_set_nonblock((long)w, 1);
    }
    return 0;
}

long __quark_socketpair(long domain, long type, long protocol, int *sv) {
    /* Only a local stream, which is what a Wayland connection is. */
    if (domain != AF_UNIX || (type & 0xF) != SOCK_STREAM || protocol != 0) {
        return -LX_ENOSYS;
    }
    if (!sv) {
        return -LX_EFAULT;
    }
    unsigned long r = __syscall0(SYS_SOCKETPAIR);
    if (r == QUARK_ERR) {
        return -LX_EMFILE;
    }
    sv[0] = (int)(r >> 32);
    sv[1] = (int)(r & 0xFFFFFFFF);
    return 0;
}

/* A control message's length, rounded up as the next one begins. */
#define CMSG_ALIGN(n) (((n) + 7) & ~7UL)

/* Who sent a message, as Linux's struct ucred says it. */
struct lx_ucred {
    int pid;
    unsigned int uid;
    unsigned int gid;
};

/* The descriptors a message's control part carries, from every SCM_RIGHTS
   in it, into `fds`: how many, or a negative errno — too many for one send
   is ETOOMANYREFS, and credentials that are not the sender's own EPERM. */
static long control_fds(const struct msghdr *m, unsigned int *fds) {
    if (!m->msg_control || m->msg_controllen < sizeof(struct cmsghdr)) {
        return 0;
    }
    const char *at = m->msg_control;
    const char *end = at + m->msg_controllen;
    long n = 0;
    while (at + sizeof(struct cmsghdr) <= end) {
        const struct cmsghdr *c = (const struct cmsghdr *)at;
        if (c->cmsg_len < sizeof(struct cmsghdr) || at + c->cmsg_len > end) {
            return -LX_EINVAL;
        }
        const char *data = at + sizeof(struct cmsghdr);
        unsigned long payload = c->cmsg_len - sizeof(struct cmsghdr);
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
            for (unsigned long i = 0; i + sizeof(int) <= payload; i += sizeof(int)) {
                if (n == FDS_AT_ONCE) {
                    return -LX_ETOOMANYREFS;
                }
                fds[n++] = (unsigned int)*(const int *)(data + i);
            }
        } else if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS) {
            /* A sender may say only who it is: the receiver is told that
               anyway, by the kernel. */
            const struct lx_ucred *cr = (const struct lx_ucred *)data;
            unsigned long ids = __syscall0(SYS_GET_UID);
            if (payload < sizeof *cr || cr->pid != (int)__syscall0(SYS_PID) ||
                cr->uid != (unsigned int)(ids >> 32) || cr->gid != (unsigned int)ids) {
                return -LX_EPERM;
            }
        }
        at += CMSG_ALIGN(c->cmsg_len);
    }
    return n;
}

long __quark_sendmsg(long fd, const void *msg, long flags) {
    /* Either the call or the descriptor may ask not to wait, and both mean the
       same thing to the kernel. */
    unsigned long fl = ((flags & MSG_DONTWAIT) || __quark_fd_is_nonblock(fd))
                           ? QUARK_DONTWAIT
                           : 0;
    const struct msghdr *m = msg;
    if (!m) {
        return -LX_EFAULT;
    }
    unsigned int pass[FDS_AT_ONCE];
    long npass = control_fds(m, pass);
    if (npass < 0) {
        return npass;
    }

    long total = 0;
    for (unsigned long i = 0; i < m->msg_iovlen; i++) {
        const struct iovec *v = &m->msg_iov[i];
        if (v->iov_len == 0) {
            continue;
        }
        /* The descriptors ride with the first piece that carries bytes, so
           they are queued before anything the peer can read. */
        unsigned long many = (total == 0 && npass > 0) ? QUARK_FD_MANY | (unsigned long)npass << 8 : 0;
        unsigned long w = __syscall5(SYS_FD_SEND, (unsigned long)fd, (unsigned long)v->iov_base, v->iov_len,
                                     many ? (unsigned long)pass : QUARK_ERR, fl | many);
        /* Cut short by a signal with nothing sent, the descriptor included:
           sent again, or EINTR, as the handler asked. */
        long cut = quark_cut_short(w, 1);
        if (cut > 0) {
            i--;
            continue;
        }
        if (cut < 0) {
            return total ? total : cut;
        }
        if (w == QUARK_ERR) {
            if (total) {
                return total;
            }
            /* Nobody at the other end is a broken pipe, and a signal unless
               the call asked for none. */
            unsigned long k = __syscall1(SYS_FD_KIND, (unsigned long)fd);
            if (k == QUARK_ERR) {
                return -LX_EBADF;
            }
            if (k & QUARK_FD_GONE) {
                if (!(flags & MSG_NOSIGNAL)) {
                    __quark_sig_pipe();
                }
                return -LX_EPIPE;
            }
            if (QUARK_FD_KIND(k) == QUARK_FD_KIND_LOCAL) {
                return -LX_ENOTCONN;
            }
            if (QUARK_FD_KIND(k) != QUARK_FD_KIND_STREAM) {
                return -LX_ENOTSOCK;
            }
            /* Room for the bytes, and none for the descriptors. */
            return npass > 0 ? -LX_ETOOMANYREFS : -LX_EIO;
        }
        if (w == QUARK_WOULD_BLOCK) {
            return total ? total : -LX_EAGAIN;
        }
        total += (long)(w & 0xFFFFFFFF);
        if ((unsigned long)(w & 0xFFFFFFFF) < v->iov_len) {
            break;
        }
    }
    /* A control message with no data still has to hand the descriptors over. */
    if (total == 0 && npass > 0) {
        unsigned long w = __syscall5(SYS_FD_SEND, (unsigned long)fd, 0, 0, (unsigned long)pass,
                                     fl | QUARK_FD_MANY | (unsigned long)npass << 8);
        if (w == QUARK_ERR) {
            return -LX_ETOOMANYREFS;
        }
    }
    return total;
}

long __quark_recvmsg(long fd, void *msg, long flags) {
    unsigned long fl = ((flags & MSG_DONTWAIT) || __quark_fd_is_nonblock(fd))
                           ? QUARK_DONTWAIT
                           : 0;
    struct msghdr *m = msg;
    if (!m) {
        return -LX_EFAULT;
    }

    /* What the control part has room for: who sent it, if the socket asked
       to be told (SO_PASSCRED), and then as many descriptors as fit. The
       kernel takes as many as there are and that room — up to 32 — and
       puts each in the lowest free slot from 3. */
    unsigned long space = m->msg_control ? m->msg_controllen : 0;
    int creds = space >= sizeof(struct cmsghdr) &&
                __syscall3(SYS_SOCKET_OPTION, (unsigned long)fd, 0, ~0UL) == 1;
    unsigned long creds_space = sizeof(struct cmsghdr) + CMSG_ALIGN(sizeof(struct lx_ucred));
    unsigned long left = creds ? (space >= creds_space ? space - creds_space : 0) : space;
    unsigned long room = left > sizeof(struct cmsghdr) ? (left - sizeof(struct cmsghdr)) / sizeof(int) : 0;
    if (room > FDS_AT_ONCE) {
        room = FDS_AT_ONCE;
    }

    long total = 0;
    unsigned int landed[FDS_AT_ONCE];
    unsigned long nlanded = 0;
    for (unsigned long i = 0; i < m->msg_iovlen; i++) {
        struct iovec *v = &m->msg_iov[i];
        if (v->iov_len == 0) {
            continue;
        }
        /* Only the first read may collect descriptors: a later piece asking
           would take the next message's before its bytes. */
        unsigned long many = (total == 0 && room > 0) ? QUARK_FD_MANY | room << 8 : 0;
        unsigned long r = __syscall5(SYS_FD_RECV, (unsigned long)fd, (unsigned long)v->iov_base, v->iov_len,
                                     many ? (unsigned long)landed : QUARK_ERR, fl | many);
        /* Cut short by a signal with nothing taken, descriptors included. */
        long cut = quark_cut_short(r, 1);
        if (cut > 0) {
            i--;
            continue;
        }
        if (cut < 0) {
            if (total) {
                break;
            }
            return cut;
        }
        if (r == QUARK_ERR) {
            if (total) {
                break;
            }
            unsigned long k = __syscall1(SYS_FD_KIND, (unsigned long)fd);
            return k == QUARK_ERR ? -LX_EBADF
                   : QUARK_FD_KIND(k) == QUARK_FD_KIND_LOCAL ? -LX_ENOTCONN
                   : QUARK_FD_KIND(k) != QUARK_FD_KIND_STREAM ? -LX_ENOTSOCK
                                                              : -LX_EIO;
        }
        if (r == QUARK_WOULD_BLOCK) {
            /* Nothing there. A caller that loops until this happens -- which
               is what libwayland does -- is the reason MSG_DONTWAIT cannot be
               ignored: the last turn of the loop is always this one. */
            if (total) {
                break;
            }
            m->msg_controllen = 0;
            return -LX_EAGAIN;
        }
        if (many) {
            nlanded = r >> 32;
            /* Whatever arrived, each number now names it: a file as easily
               as memory. */
            for (unsigned long k = 0; k < nlanded; k++) {
                __quark_fd_forget((long)landed[k]);
            }
        }
        unsigned long n = r & 0xFFFFFFFF;
        total += (long)n;
        if (n < v->iov_len) {
            break;
        }
    }

    char *at = m->msg_control;
    unsigned long used = 0;
    m->msg_flags = 0;
    if (creds) {
        if (space < creds_space) {
            m->msg_flags |= MSG_CTRUNC;
        } else {
            unsigned int who[3] = {0, 0, 0};
            __syscall2(SYS_SOCKET_PEER, (unsigned long)fd, (unsigned long)who);
            struct cmsghdr *c = (struct cmsghdr *)at;
            c->cmsg_len = sizeof(struct cmsghdr) + sizeof(struct lx_ucred);
            c->cmsg_level = SOL_SOCKET;
            c->cmsg_type = SCM_CREDENTIALS;
            struct lx_ucred *cr = (struct lx_ucred *)(at + sizeof(struct cmsghdr));
            cr->pid = (int)who[0];
            cr->uid = who[1];
            cr->gid = who[2];
            used = creds_space;
        }
    }
    if (nlanded) {
        struct cmsghdr *c = (struct cmsghdr *)(at + used);
        c->cmsg_len = sizeof(struct cmsghdr) + nlanded * sizeof(int);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        int *fds = (int *)(at + used + sizeof(struct cmsghdr));
        for (unsigned long k = 0; k < nlanded; k++) {
            fds[k] = (int)landed[k];
        }
        used += CMSG_ALIGN(c->cmsg_len);
    }
    m->msg_controllen = used;
    return total;
}

/* ------------------------------------------------------------------------ */
/* A socket of the local family, by a name.                                 */
/* ------------------------------------------------------------------------ */

/* A local socket that is nothing yet is the kernel's (SYS_SOCKET). The file
   server gives one a name — an inode whose mode says it is a socket — and
   connects one to whatever listens at a name, having decided by the name's
   owner and mode whether this program may. Connected, it is a stream, as a
   pair's is, and everything a stream does it does. A name in the abstract
   namespace, and a socket of any other kind than a stream, are refused. */
#define LX_SOCK_NONBLOCK 04000
#define LX_SOCK_CLOEXEC  02000000
#define LX_SO_TYPE       3
#define LX_SO_ERROR      4
#define LX_SO_SNDBUF     7
#define LX_SO_RCVBUF     8
#define LX_SO_PASSCRED   16
#define LX_SO_PEERCRED   17
#define LX_SO_RCVTIMEO   20
#define LX_SO_SNDTIMEO   21
#define LX_SO_ACCEPTCONN 30
#define LX_SO_PROTOCOL   38
#define LX_SO_DOMAIN     39
#define SUN_PATH         108

struct lx_sockaddr_un {
    unsigned short sun_family;
    char sun_path[SUN_PATH];
};

/* What this program named its sockets and connected them to, for
   getsockname and getpeername: the names are the file server's, and the
   kernel keeps neither. Forgotten at exec, as a C library's memory is. */
static char named_as[MAX_FDS][SUN_PATH];
static char connected_to[MAX_FDS][SUN_PATH];

static unsigned long kind_of(long fd) {
    unsigned long k = __syscall1(SYS_FD_KIND, (unsigned long)fd);
    return k == QUARK_ERR ? 0 : QUARK_FD_KIND(k);
}

/* Why a call on `fd` that wanted a socket was refused, by what it is. */
static long not_a_socket(long fd, long if_local, long if_stream) {
    switch (kind_of(fd)) {
    case 0: return -LX_EBADF;
    case QUARK_FD_KIND_LOCAL: return if_local;
    case QUARK_FD_KIND_STREAM: return if_stream;
    default: return -LX_ENOTSOCK;
    }
}

/* The path in a local address, NUL-ended into `path`, or a negative errno:
   a name in the abstract namespace (a nought first), or none at all, is
   EINVAL. */
static long path_of(const void *addr, unsigned long len, char *path) {
    const struct lx_sockaddr_un *a = addr;
    if (!a) {
        return -LX_EFAULT;
    }
    if (len <= sizeof a->sun_family || a->sun_family != AF_UNIX) {
        return -LX_EINVAL;
    }
    unsigned long n = len - sizeof a->sun_family;
    if (n > SUN_PATH) {
        n = SUN_PATH;
    }
    unsigned long i = 0;
    while (i < n && a->sun_path[i]) {
        path[i] = a->sun_path[i];
        i++;
    }
    if (i == 0) {
        return -LX_EINVAL;
    }
    if (i == SUN_PATH) {
        return -LX_ENAMETOOLONG;
    }
    path[i] = 0;
    return 0;
}

static void remember(char *slot, const char *path) {
    unsigned long i = 0;
    while (path && path[i] && i < SUN_PATH - 1) {
        slot[i] = path[i];
        i++;
    }
    slot[i] = 0;
}

long __quark_socket(long domain, long type, long protocol) {
    /* Every other family is not supported, which is the answer a program
       has something to do about: musl asks a name service daemon who a
       user is, and takes the local family's ENOENT, or another's
       EAFNOSUPPORT, for there being none. */
    if (domain != AF_UNIX) {
        return -LX_EAFNOSUPPORT;
    }
    if ((type & 0xF) != SOCK_STREAM || protocol != 0) {
        return -LX_EPROTONOSUPPORT;
    }
    unsigned long fd = __syscall1(SYS_SOCKET, 0);
    if (fd == QUARK_ERR) {
        return -LX_EMFILE;
    }
    if (type & LX_SOCK_NONBLOCK) {
        __quark_fd_set_nonblock((long)fd, 1);
    }
    if (type & LX_SOCK_CLOEXEC) {
        __syscall3(SYS_FD_FLAGS, fd, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
    }
    named_as[fd][0] = 0;
    connected_to[fd][0] = 0;
    return (long)fd;
}

long __quark_bind(long fd, const void *addr, unsigned long len) {
    char path[SUN_PATH];
    long bad = path_of(addr, len, path);
    if (bad) {
        return bad;
    }
    unsigned long mask = __syscall1(SYS_UMASK, ~0UL);
    int err = quark_vfs_bind(0, path, fd, 0777 & ~mask);
    if (err == QUARK_VFS_EXISTS || err == QUARK_VFS_BUSY) {
        return -LX_EADDRINUSE;
    }
    if (err == QUARK_VFS_INVALID_HANDLE) {
        return not_a_socket(fd, -LX_EINVAL, -LX_EINVAL);
    }
    if (err) {
        return __quark_vfs_errno(err);
    }
    if (fd >= 0 && fd < MAX_FDS) {
        remember(named_as[fd], path);
    }
    return 0;
}

long __quark_listen(long fd, long backlog) {
    if (__syscall2(SYS_SOCKET_LISTEN, (unsigned long)fd, backlog < 0 ? 0 : (unsigned long)backlog) == QUARK_ERR) {
        /* A socket with no name, or one connected already. */
        return not_a_socket(fd, -LX_EINVAL, -LX_EINVAL);
    }
    return 0;
}

/* An address of the local family with no name: what a connector that never
   bound is, as Linux says it. */
static void unnamed(void *addr, unsigned int *len, const char *path) {
    if (!addr || !len) {
        return;
    }
    struct lx_sockaddr_un a;
    a.sun_family = AF_UNIX;
    unsigned long n = 0;
    while (path && path[n] && n < SUN_PATH - 1) {
        a.sun_path[n] = path[n];
        n++;
    }
    unsigned int full = (unsigned int)(sizeof a.sun_family + (n ? n + 1 : 0));
    if (n) {
        a.sun_path[n] = 0;
    }
    unsigned int copy = *len < full ? *len : full;
    const char *from = (const char *)&a;
    for (unsigned int i = 0; i < copy; i++) {
        ((char *)addr)[i] = from[i];
    }
    *len = full;
}

long __quark_accept(long fd, void *addr, unsigned int *len, long flags) {
    unsigned long r;
    for (;;) {
        r = __syscall2(SYS_SOCKET_ACCEPT, (unsigned long)fd, __quark_fd_is_nonblock(fd) ? 1 : 0);
        long cut = quark_cut_short(r, 1);
        if (cut < 0) {
            return cut;
        }
        if (!cut) {
            break;
        }
    }
    if (r == QUARK_WOULD_BLOCK) {
        return -LX_EAGAIN;
    }
    if (r == QUARK_ERR) {
        /* Not listening; or listening, with nowhere to put a connection. */
        long why = not_a_socket(fd, -LX_EINVAL, -LX_EINVAL);
        if (why == -LX_EINVAL && __syscall3(SYS_SOCKET_OPTION, (unsigned long)fd, 0, ~0UL) != QUARK_ERR &&
            kind_of(fd) == QUARK_FD_KIND_LOCAL) {
            return -LX_EMFILE;
        }
        return why;
    }
    __quark_fd_forget((long)r);
    if (flags & LX_SOCK_NONBLOCK) {
        __quark_fd_set_nonblock((long)r, 1);
    }
    if (flags & LX_SOCK_CLOEXEC) {
        __syscall3(SYS_FD_FLAGS, r, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
    }
    if (r < MAX_FDS) {
        remember(named_as[r], fd >= 0 && fd < MAX_FDS ? named_as[fd] : "");
        connected_to[r][0] = 0;
    }
    unnamed(addr, len, "");
    return (long)r;
}

long __quark_connect(long fd, const void *addr, unsigned long len) {
    char path[SUN_PATH];
    long bad = path_of(addr, len, path);
    if (bad) {
        return bad;
    }
    for (;;) {
        int err = quark_vfs_connect(0, path, fd);
        if (!err) {
            break;
        }
        if (err == QUARK_VFS_NO_PEER) {
            return -LX_ECONNREFUSED;
        }
        if (err == QUARK_VFS_INVALID_HANDLE) {
            return not_a_socket(fd, -LX_EINVAL, -LX_EISCONN);
        }
        if (err != QUARK_VFS_WOULD_BLOCK) {
            return __quark_vfs_errno(err);
        }
        /* The listener has as many waiting as it has room for. A socket that
           may not wait says so; one that may waits, as Linux's does, a
           little at a time, and a handler that runs ends the wait. */
        if (__quark_fd_is_nonblock(fd)) {
            return -LX_EAGAIN;
        }
        unsigned long slept = __syscall3(SYS_SIG_WAIT, 0, quark_span(10000000UL), 0);
        if (quark_cut_short(slept, 0) < 0) {
            return -LX_EINTR;
        }
    }
    if (fd >= 0 && fd < MAX_FDS) {
        remember(connected_to[fd], path);
    }
    return 0;
}

long __quark_sockname(long fd, void *addr, unsigned int *len, int peer) {
    if (!addr || !len) {
        return -LX_EFAULT;
    }
    unsigned long k = kind_of(fd);
    if (k == 0) {
        return -LX_EBADF;
    }
    if (k != QUARK_FD_KIND_LOCAL && k != QUARK_FD_KIND_STREAM) {
        return -LX_ENOTSOCK;
    }
    if (peer && k != QUARK_FD_KIND_STREAM) {
        return -LX_ENOTCONN;
    }
    const char *path = fd < MAX_FDS ? (peer ? connected_to[fd] : named_as[fd]) : "";
    unnamed(addr, len, path);
    return 0;
}

long __quark_getsockopt(long fd, long level, long name, void *val, unsigned int *len) {
    unsigned long k = kind_of(fd);
    if (k == 0) {
        return -LX_EBADF;
    }
    if (k != QUARK_FD_KIND_LOCAL && k != QUARK_FD_KIND_STREAM) {
        return -LX_ENOTSOCK;
    }
    if (!val || !len) {
        return -LX_EFAULT;
    }
    if (level != SOL_SOCKET) {
        return -LX_ENOPROTOOPT;
    }
    int answer;
    switch (name) {
    case LX_SO_PEERCRED: {
        unsigned int who[3];
        if (k != QUARK_FD_KIND_STREAM || __syscall2(SYS_SOCKET_PEER, (unsigned long)fd, (unsigned long)who) != 0) {
            return -LX_ENOTCONN;
        }
        struct lx_ucred cr = {(int)who[0], who[1], who[2]};
        unsigned int copy = *len < sizeof cr ? *len : (unsigned int)sizeof cr;
        for (unsigned int i = 0; i < copy; i++) {
            ((char *)val)[i] = ((const char *)&cr)[i];
        }
        *len = copy;
        return 0;
    }
    case LX_SO_PASSCRED:
        answer = __syscall3(SYS_SOCKET_OPTION, (unsigned long)fd, 0, ~0UL) == 1;
        break;
    case LX_SO_TYPE: answer = SOCK_STREAM; break;
    case LX_SO_DOMAIN: answer = AF_UNIX; break;
    case LX_SO_PROTOCOL: answer = 0; break;
    case LX_SO_ERROR: answer = 0; break;
    case LX_SO_ACCEPTCONN: answer = 0; break;
    case LX_SO_SNDBUF:
    case LX_SO_RCVBUF: answer = 4096; break;
    case LX_SO_RCVTIMEO:
    case LX_SO_SNDTIMEO: {
        /* No timeout: a timeval of noughts. */
        unsigned int copy = *len < 16 ? *len : 16;
        for (unsigned int i = 0; i < copy; i++) {
            ((char *)val)[i] = 0;
        }
        *len = copy;
        return 0;
    }
    default:
        return -LX_ENOPROTOOPT;
    }
    if (*len < sizeof(int)) {
        return -LX_EINVAL;
    }
    *(int *)val = answer;
    *len = sizeof(int);
    return 0;
}

long __quark_setsockopt(long fd, long level, long name, const void *val, unsigned long len) {
    unsigned long k = kind_of(fd);
    if (k == 0) {
        return -LX_EBADF;
    }
    if (k != QUARK_FD_KIND_LOCAL && k != QUARK_FD_KIND_STREAM) {
        return -LX_ENOTSOCK;
    }
    if (level != SOL_SOCKET) {
        return -LX_ENOPROTOOPT;
    }
    switch (name) {
    case LX_SO_PASSCRED:
        if (!val || len < sizeof(int)) {
            return -LX_EINVAL;
        }
        return __syscall3(SYS_SOCKET_OPTION, (unsigned long)fd, 0, *(const int *)val ? 1 : 0) == QUARK_ERR
                   ? -LX_EINVAL
                   : 0;
    /* Taken and not kept: the buffers are a stream's own, and nothing here
       times a socket out. */
    case LX_SO_SNDBUF:
    case LX_SO_RCVBUF:
    case LX_SO_RCVTIMEO:
    case LX_SO_SNDTIMEO:
    case 2:  /* SO_REUSEADDR */
    case 9:  /* SO_KEEPALIVE */
    case 13: /* SO_LINGER */
        return 0;
    default:
        return -LX_ENOPROTOOPT;
    }
}

/* send and recv are these, in musl: a message of one piece, with no address
   to give or be told for a stream that is connected. */
long __quark_sendto(long fd, const void *buf, unsigned long len, long flags, const void *addr, unsigned long alen) {
    if (addr || alen) {
        return not_a_socket(fd, -LX_ENOTCONN, -LX_EISCONN);
    }
    struct iovec v = {(void *)buf, len};
    struct msghdr m = {NULL, 0, &v, 1, NULL, 0, 0};
    return __quark_sendmsg(fd, &m, flags);
}

long __quark_recvfrom(long fd, void *buf, unsigned long len, long flags, void *addr, unsigned int *alen) {
    struct iovec v = {buf, len};
    struct msghdr m = {NULL, 0, &v, 1, NULL, 0, 0};
    long n = __quark_recvmsg(fd, &m, flags);
    if (n >= 0 && addr && alen) {
        unnamed(addr, alen, fd >= 0 && fd < MAX_FDS ? connected_to[fd] : "");
    }
    return n;
}

long __quark_poll(void *fds, long nfds, long timeout_ns, const unsigned long *under) {
    struct lx_pollfd *p = fds;
    if (nfds < 0 || (!p && nfds > 0)) {
        return -LX_EFAULT;
    }
    /* Waiting on nothing at all is a sleep, and `poll(NULL, 0, ms)` is how a
       main loop whose sources are all timeouts spends its time. Refusing it
       because the array is null turned that wait into a spin. */
    if (nfds > MAX_POLL) {
        return -LX_EINVAL;
    }

    struct qw_pollfd q[MAX_POLL];
    for (long i = 0; i < nfds; i++) {
        q[i].fd = (unsigned int)p[i].fd;
        q[i].events = 0;
        q[i].revents = 0;
        q[i].pad = 0;
        if (p[i].events & LX_POLLIN) {
            q[i].events |= QW_READABLE;
        }
        if (p[i].events & LX_POLLOUT) {
            q[i].events |= QW_WRITABLE;
        }
    }

    /* A negative timeout means wait for ever, which here is as long as
       there is. */
    unsigned long span = timeout_ns < 0 ? ~0UL : quark_span((unsigned long)timeout_ns);
    unsigned long deadline = timeout_ns < 0 ? 0 : quark_now() + (unsigned long)timeout_ns;
    unsigned long n;
    for (;;) {
        /* Under another mask, it is put on in the same step as the wait
           begins and comes off as it ends: ppoll and pselect. */
        n = __syscall5(SYS_POLL, (unsigned long)q, (unsigned long)nfds, span,
                       under ? *under : 0, under ? QUARK_POLL_UNDER : 0);
        /* A signal ended the wait. A handler that ran makes that the answer,
           whatever it asked for: a poll is never made again. One that ran
           nothing leaves what is left of the wait to do. */
        long cut = quark_cut_short(n, 0);
        if (cut < 0) {
            return cut;
        }
        if (!cut) {
            break;
        }
        if (timeout_ns >= 0) {
            unsigned long now = quark_now();
            span = quark_span(now < deadline ? deadline - now : 0);
        }
    }
    if (n == QUARK_ERR) {
        return -LX_EINVAL;
    }
    for (long i = 0; i < nfds; i++) {
        short rev = 0;
        if (q[i].revents & QW_READABLE) {
            rev |= LX_POLLIN;
        }
        if (q[i].revents & QW_WRITABLE) {
            rev |= LX_POLLOUT;
        }
        if (q[i].revents & QW_HANGUP) {
            rev |= LX_POLLHUP;
        }
        if (q[i].revents & QW_INVALID) {
            rev |= LX_POLLNVAL;
        }
        p[i].revents = rev;
    }
    return (long)n;
}

#define LX_EPOLL_CLOEXEC 02000000

long __quark_epoll_create(long flags) {
    if (flags & ~(long)LX_EPOLL_CLOEXEC) {
        return -LX_EINVAL;
    }
    unsigned long fd = __syscall0(SYS_POLLSET_CREATE);
    if (fd == QUARK_ERR) {
        return -LX_EMFILE;
    }
    if (flags & LX_EPOLL_CLOEXEC) {
        __syscall3(SYS_FD_FLAGS, fd, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
    }
    return (long)fd;
}

/* Linux's struct epoll_event is packed on x86-64: a u32 of events and eight
   bytes of user data with no padding between them. */
struct lx_epoll_event {
    unsigned int events;
    unsigned long long data;
} __attribute__((packed));

#define LX_EPOLL_CTL_ADD 1
#define LX_EPOLL_CTL_DEL 2
#define LX_EPOLL_CTL_MOD 3

/* epoll's own bits, beside poll's. */
#define LX_EPOLLRDNORM    0x040
#define LX_EPOLLWRNORM    0x100
#define LX_EPOLLRDHUP     0x2000
#define LX_EPOLLEXCLUSIVE (1u << 28)
#define LX_EPOLLONESHOT   (1u << 30)
#define LX_EPOLLET        (1u << 31)

/* Whether a descriptor is there: its flags can be read. */
static int is_there(long fd) {
    return fd >= 0 && __syscall3(SYS_FD_FLAGS, (unsigned long)fd, QUARK_FD_GETFLAGS, 0) != QUARK_ERR;
}

/* The kernel's set is epoll's: edge-triggered and one-shot watches, a set
   watched by a set, and why it would not, as Linux says it. A file answers
   at once whichever way it is asked, and Linux will not watch one: EPERM,
   as for memory the kernel refuses. */
long __quark_epoll_ctl(long epfd, long op, long fd, void *event) {
    struct lx_epoll_event *e = event;
    unsigned long qop;
    switch (op) {
    case LX_EPOLL_CTL_ADD: qop = 0; break;
    case LX_EPOLL_CTL_MOD: qop = 1; break;
    case LX_EPOLL_CTL_DEL: qop = 2; break;
    default: return -LX_EINVAL;
    }
    if (!is_there(epfd) || !is_there(fd)) {
        return -LX_EBADF;
    }
    if (__quark_fd_is_file(fd)) {
        return -LX_EPERM;
    }

    unsigned long events = 0;
    unsigned long token = 0;
    if (qop != 2) {
        if (!e) {
            return -LX_EFAULT;
        }
        unsigned int ev = e->events;
        if ((ev & LX_EPOLLEXCLUSIVE) && qop == 1) {
            return -LX_EINVAL;
        }
        if (ev & (LX_POLLIN | LX_EPOLLRDNORM)) {
            events |= QW_READABLE;
        }
        if (ev & (LX_POLLOUT | LX_EPOLLWRNORM)) {
            events |= QW_WRITABLE;
        }
        if (ev & LX_EPOLLRDHUP) {
            events |= QUARK_POLL_PEER_GONE;
        }
        if (ev & LX_EPOLLET) {
            events |= QUARK_POLL_EDGE;
        }
        if (ev & LX_EPOLLONESHOT) {
            events |= QUARK_POLL_ONCE;
        }
        token = (unsigned long)e->data;
    }

    unsigned long r = __syscall5(SYS_POLLSET_CTL, (unsigned long)epfd, qop | QUARK_POLLSET_WHY,
                                 (unsigned long)fd, events, token);
    switch (r) {
    case 0:                    return 0;
    case QUARK_POLLSET_EXISTS: return -LX_EEXIST;
    case QUARK_POLLSET_ABSENT: return -LX_ENOENT;
    case QUARK_POLLSET_CANNOT: return -LX_EPERM;
    case QUARK_POLLSET_LOOP:   return -LX_ELOOP;
    case QUARK_POLLSET_FULL:   return -LX_ENOSPC;
    default:                   return -LX_EINVAL;
    }
}

struct qw_ready {
    unsigned long token;
    unsigned int events;
    unsigned int pad;
};

long __quark_epoll_wait(long epfd, void *events, long maxevents, long timeout_ns,
                        const unsigned long *under) {
    struct lx_epoll_event *out = events;
    if (!out || maxevents <= 0) {
        return -LX_EINVAL;
    }
    if (maxevents > MAX_POLL) {
        maxevents = MAX_POLL;
    }

    struct qw_ready ready[MAX_POLL];
    unsigned long span = timeout_ns < 0 ? ~0UL : quark_span((unsigned long)timeout_ns);
    unsigned long deadline = timeout_ns < 0 ? 0 : quark_now() + (unsigned long)timeout_ns;
    unsigned long n;
    for (;;) {
        n = __syscall5(SYS_POLLSET_WAIT, (unsigned long)epfd, (unsigned long)ready,
                       (unsigned long)maxevents, span, under ? *under | QUARK_POLLSET_UNDER : 0);
        long cut = quark_cut_short(n, 0);
        if (cut < 0) {
            return cut;
        }
        if (!cut) {
            break;
        }
        if (timeout_ns >= 0) {
            unsigned long now = quark_now();
            span = quark_span(now < deadline ? deadline - now : 0);
        }
    }
    if (n == QUARK_ERR) {
        return -LX_EINVAL;
    }
    for (unsigned long i = 0; i < n; i++) {
        unsigned int ev = 0;
        if (ready[i].events & QW_READABLE) {
            ev |= LX_POLLIN;
        }
        if (ready[i].events & QW_WRITABLE) {
            ev |= LX_POLLOUT;
        }
        if (ready[i].events & QW_HANGUP) {
            ev |= LX_POLLHUP;
        }
        if (ready[i].events & QUARK_POLL_PEER_GONE) {
            ev |= LX_EPOLLRDHUP;
        }
        out[i].events = ev;
        out[i].data = ready[i].token;
    }
    return (long)n;
}
