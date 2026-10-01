/* The paths and the requests a terminal uses to get a pseudo-terminal.
 *
 * musl's `openpty` opens `/dev/ptmx`, asks it which pty it was given
 * (`TIOCGPTN`), unlocks it (`TIOCSPTLCK`) and opens `/dev/pts/N`. None of
 * those are files: a pty is a pair of kernel descriptors, so the paths are
 * caught here, ahead of the VFS, the way a Linux kernel catches them ahead of
 * its filesystems.
 *
 * `TIOCSPTLCK` is accepted and does nothing. Linux locks a slave until its
 * master says otherwise, so that nobody opens the slave of a pty that is
 * mid-allocation; here a slave can only be opened by number and only while its
 * master is held, which is the same guarantee arrived at from the other side.
 */

#include <quark/syscall.h>

#include "abi.h"

/* `SYS_PTY_CTL` operations, as the kernel numbers them. */
#define PTY_GET_TERMIOS 0
#define PTY_SET_TERMIOS 1
#define PTY_GET_WINSIZE 2
#define PTY_SET_WINSIZE 3
#define PTY_NUMBER      4
#define PTY_SET_FRONT   5
#define PTY_GET_FRONT   6
#define PTY_SET_SESSION 7
#define PTY_GET_SESSION 8
/* With PTY_SET_FRONT: the caller is not to be stopped for asking. */
#define PTY_FRONT_QUIETLY (1UL << 63)

/* The requests a terminal emulator and a shell actually send. */
#define TCGETS     0x5401
#define TCSETS     0x5402
#define TCSETSW    0x5403
#define TCSETSF    0x5404
#define TIOCSCTTY  0x540E
#define TIOCGPGRP  0x540F
#define TIOCSPGRP  0x5410
#define TIOCNOTTY  0x5422
#define TIOCGSID   0x5429
#define LX_SIGTTOU 22
#define TIOCGWINSZ 0x5413
#define TIOCSWINSZ 0x5414
#define TIOCGPTN   0x80045430
#define TIOCSPTLCK 0x40045431

static int streq(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

/* "/dev/pts/12" -> 12, or -1 for anything else. */
static long pts_number(const char *path) {
    const char *p = path;
    const char *want = "/dev/pts/";
    while (*want) {
        if (*p != *want) {
            return -1;
        }
        p++;
        want++;
    }
    if (!*p) {
        return -1;
    }
    long n = 0;
    for (; *p; p++) {
        if (*p < '0' || *p > '9') {
            return -1;
        }
        n = n * 10 + (*p - '0');
        if (n > 4096) {
            return -1;
        }
    }
    return n;
}

/* Whether this path is a terminal rather than a file, and the descriptor for
   it if so. Returns -1 when the path is not one of ours, which is not an
   error: the caller goes on to the VFS. */
long __quark_pty_open(const char *path) {
    if (!path) {
        return -1;
    }
    if (streq(path, "/dev/ptmx")) {
        unsigned long fd = __syscall0(SYS_PTY_CREATE);
        return fd == QUARK_ERR ? -LX_ENOSPC : (long)fd;
    }
    long n = pts_number(path);
    if (n < 0) {
        return -1;
    }
    unsigned long fd = __syscall1(SYS_PTY_OPEN, (unsigned long)n);
    return fd == QUARK_ERR ? -LX_ENOENT : (long)fd;
}

/* A terminal opened by the leader of a session that has no terminal becomes
   that session's, unless the open said O_NOCTTY: Linux's rule, and what a
   getty relies on. The kernel refuses for anybody else, which is the test. */
void __quark_pty_opened(long fd, int noctty) {
    if (!noctty && __quark_pty_slave_number(fd) >= 0) {
        __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_SET_SESSION, 0);
    }
}

/* `/dev/pts` itself, for a program that stats it before using it. */
int __quark_pty_path(const char *path) {
    return path && (streq(path, "/dev/ptmx") || pts_number(path) >= 0);
}

/* Which terminal descriptor `fd` is the slave of, or -1 if it is not one. */
long __quark_pty_slave_number(long fd) {
    if (fd < 0 || fd >= MAX_FDS) {
        return -1;
    }
    unsigned long kind = __syscall1(SYS_FD_KIND, (unsigned long)fd);
    if (kind == QUARK_ERR || QUARK_FD_KIND(kind) != QUARK_FD_KIND_PTY_SLAVE) {
        return -1;
    }
    unsigned long number = __syscall3(SYS_PTY_CTL, (unsigned long)fd, 4, 0);
    return number == QUARK_ERR ? -1 : (long)number;
}

/* A descriptor this program has for the terminal `path` names, or -1.
 *
 * What a `stat` of `/dev/pts/N` is answered from. Opening the terminal to ask
 * about it would be a slave opened and closed, and the last slave closing is
 * how a terminal's master is told its session has ended; so the only
 * terminals a program can ask about by name are the ones it already has —
 * which is all `ttyname` does. */
long __quark_pty_held(const char *path) {
    long number = path ? pts_number(path) : -1;
    if (number < 0) {
        return -1;
    }
    for (long fd = 0; fd < MAX_FDS; fd++) {
        if (__quark_pty_slave_number(fd) == number) {
            return fd;
        }
    }
    return -1;
}

/* The requests a terminal makes of a descriptor.
 *
 * Anything this does not answer is `ENOTTY`, which is the right answer to a
 * program asking a pipe about its window size — and is how `isatty` tells the
 * two apart. */
long __quark_ioctl(long fd, unsigned long request, unsigned long arg) {
    switch (request) {
    case TCGETS:
        return __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_GET_TERMIOS, arg) == QUARK_ERR
                   ? -LX_ENOTTY
                   : 0;
    case TCSETS:
    case TCSETSW:
    case TCSETSF:
        /* The three differ in when they take effect: now, after what has been
           written drains, or after that and with what has been typed thrown
           away. With a buffer this small and no hardware behind it, draining
           is already done and there is nothing to discard that a program has
           not already been given. */
        return __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_SET_TERMIOS, arg) == QUARK_ERR
                   ? -LX_ENOTTY
                   : 0;
    case TIOCGWINSZ:
        return __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_GET_WINSIZE, arg) == QUARK_ERR
                   ? -LX_ENOTTY
                   : 0;
    case TIOCSWINSZ:
        return __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_SET_WINSIZE, arg) == QUARK_ERR
                   ? -LX_ENOTTY
                   : 0;
    case TIOCGPTN: {
        unsigned long n = __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_NUMBER, 0);
        if (n == QUARK_ERR) {
            return -LX_ENOTTY;
        }
        if (arg) {
            *(unsigned int *)arg = (unsigned int)n;
        }
        return 0;
    }
    case TIOCSPTLCK:
        /* Accepted and nothing done: a slave here is openable only by number
           and only while its master is held, which is the guarantee the lock
           exists to give. */
        return __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_NUMBER, 0) == QUARK_ERR
                   ? -LX_ENOTTY
                   : 0;
    case TIOCSCTTY: {
        /* The caller's session takes this terminal: what a terminal's child
           asks for right after `setsid`. */
        unsigned long r = __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_SET_SESSION, 0);
        return r == 0 ? 0 : r == QUARK_NOT_ALLOWED ? -LX_EPERM : -LX_ENOTTY;
    }
    case TIOCNOTTY:
        /* Giving up a controlling terminal is something a session does by
           its leader ending. A program that asks is about to call `setsid`,
           which is what leaves the terminal behind. */
        return __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_NUMBER, 0) == QUARK_ERR
                   ? -LX_ENOTTY
                   : 0;
    case TIOCGSID:
    case TIOCGPGRP: {
        /* tcgetsid and tcgetpgrp: whose terminal this is, and which process
           group is in front of it. Only of the caller's own terminal. */
        unsigned long r = __syscall3(SYS_PTY_CTL, (unsigned long)fd,
                                     request == TIOCGSID ? PTY_GET_SESSION : PTY_GET_FRONT, 0);
        if (r == QUARK_ERR) {
            return -LX_ENOTTY;
        }
        if (arg) {
            /* No group in front is said as a number no group has. */
            *(int *)arg = r ? (int)r : 0x7FFFFFFF;
        }
        return 0;
    }
    case TIOCSPGRP: {
        /* tcsetpgrp: put a group in front. A job in the background that
           asks is stopped for it (SIGTTOU) rather than obeyed — unless it
           has the signal blocked, which only this layer knows, so the kernel
           is told. A shell takes the terminal back exactly that way. */
        if (!arg || *(int *)arg <= 0) {
            return -LX_EINVAL;
        }
        unsigned long group = (unsigned long)*(int *)arg;
        for (;;) {
            unsigned long quietly = __quark_sig_is_blocked(LX_SIGTTOU) ? PTY_FRONT_QUIETLY : 0;
            unsigned long r = __syscall3(SYS_PTY_CTL, (unsigned long)fd, PTY_SET_FRONT,
                                         group | quietly);
            if (r == 0) {
                return 0;
            }
            if (r != QUARK_INTERRUPTED) {
                return r == QUARK_NOT_ALLOWED ? -LX_EPERM : -LX_ENOTTY;
            }
            /* Stopped, and started again; or a handler has run. Ask again,
               unless the handler wanted to be told. */
            if (__quark_sig_interrupted() & QUARK_SIG_EINTR) {
                return -LX_EINTR;
            }
        }
    }
    default:
        /* Not a terminal's question. A disk has a few of its own. */
        return __quark_blk_ioctl(fd, request, arg);
    }
}
