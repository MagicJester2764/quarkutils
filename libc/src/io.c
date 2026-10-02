/* Descriptors, and what they mean here.
 *
 * 0, 1 and 2 are the kernel's: a spawner wired them to the console and the
 * input server, and reading or writing one is a system call. Everything above
 * them is this library's own bookkeeping, because a file on Quark is a handle
 * held by the VFS rather than an object the kernel knows about. `open` asks
 * the VFS for one and remembers which descriptor number stands for it.
 *
 * The caller's buffer is lent to the VFS for each read and write, a page at
 * most, which `read` and `write` loop over.
 */

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <quark/layout.h>
#include <quark/syscall.h>
#include <quark/vfs.h>

#define PAGE_SIZE 4096

#define FIRST_FD 3
#define MAX_FILES 16

struct openfile {
    int used;
    unsigned long handle; /* what the VFS calls it */
    unsigned long offset; /* the VFS has no seek, so the position is ours */
    unsigned long size;
};

static struct openfile files[MAX_FILES];
/* The server's codes are its own; this is where they become ours. */
static int vfs_errno(int code) {
    switch (code) {
    case QUARK_VFS_NOT_FOUND:      return ENOENT;
    case QUARK_VFS_INVALID_HANDLE: return EBADF;
    case QUARK_VFS_IO:             return EIO;
    case QUARK_VFS_TOO_MANY_OPEN:  return EMFILE;
    case QUARK_VFS_INVALID_PATH:   return EINVAL;
    case QUARK_VFS_NOT_DIR:        return ENOTDIR;
    case QUARK_VFS_IS_DIR:         return EISDIR;
    case QUARK_VFS_PERMISSION:     return EACCES;
    case QUARK_VFS_READ_ONLY:      return EROFS;
    case QUARK_VFS_EXISTS:         return EEXIST;
    case QUARK_VFS_NOT_EMPTY:      return ENOTEMPTY;
    case QUARK_VFS_NOT_SUPPORTED:  return EOPNOTSUPP;
    case QUARK_VFS_NAME_TOO_LONG:  return ENAMETOOLONG;
    default:                       return EIO;
    }
}

static struct openfile *slot(int fd) {
    if (fd < FIRST_FD || fd >= FIRST_FD + MAX_FILES) {
        return 0;
    }
    struct openfile *f = &files[fd - FIRST_FD];
    return f->used ? f : 0;
}

int open(const char *path, int flags, ...) {
    int fd = -1;
    for (int i = 0; i < MAX_FILES; i++) {
        if (!files[i].used) {
            fd = FIRST_FD + i;
            break;
        }
    }
    if (fd < 0) {
        errno = EMFILE;
        return -1;
    }

    struct quark_vfs_file info;
    unsigned long how = 0;
    if (flags & O_CREAT) {
        how |= QUARK_VFS_OPEN_CREATE;
        if (flags & O_EXCL) {
            how |= QUARK_VFS_OPEN_EXCLUSIVE;
        }
    }
    if (flags & O_DIRECTORY) {
        how |= QUARK_VFS_OPEN_DIRECTORY;
    }
    if ((flags & O_TRUNC) && (flags & O_ACCMODE) != O_RDONLY) {
        how |= QUARK_VFS_OPEN_TRUNCATE;
    }
    int err = quark_vfs_open(path, how, &info);
    if (err) {
        errno = vfs_errno(err);
        return -1;
    }
    if ((flags & O_ACCMODE) != O_RDONLY && (info.is_dir || !(info.access & QUARK_VFS_W_OK))) {
        quark_vfs_close(info.handle);
        errno = info.is_dir ? EISDIR : EACCES;
        return -1;
    }

    struct openfile *f = &files[fd - FIRST_FD];
    f->used = 1;
    f->handle = info.handle;
    f->size = info.size;
    f->offset = 0;
    return fd;
}

int close(int fd) {
    struct openfile *f = slot(fd);
    if (!f) {
        errno = EBADF;
        return -1;
    }
    quark_vfs_close(f->handle);
    f->used = 0;
    return 0;
}

ssize_t read(int fd, void *buf, size_t n) {
    if (fd < FIRST_FD) {
        /* A kernel descriptor: the read is a system call. */
        unsigned long r = __syscall3(SYS_FD_READ, (unsigned long)fd, (unsigned long)buf, n);
        if (r == QUARK_ERR) {
            errno = EIO;
            return -1;
        }
        return (ssize_t)r;
    }

    struct openfile *f = slot(fd);
    if (!f) {
        errno = EBADF;
        return -1;
    }
    size_t done = 0;
    unsigned char *out = buf;
    /* One page per message, so anything larger is a loop. */
    while (done < n) {
        size_t want = n - done;
        if (want > PAGE_SIZE) {
            want = PAGE_SIZE;
        }
        unsigned long got = 0;
        /* The caller's own buffer, lent to the VFS to fill. */
        int err = quark_vfs_read(f->handle, out + done, f->offset, want, &got);
        if (err) {
            errno = vfs_errno(err);
            return done ? (ssize_t)done : -1;
        }
        if (got == 0) {
            break; /* end of file */
        }
        done += got;
        f->offset += got;
        /* A short read means the end, not a hiccup: the VFS answers from a
           page at a time and gives everything it has. */
        if (got < want) {
            break;
        }
    }
    return (ssize_t)done;
}

ssize_t write(int fd, const void *buf, size_t n) {
    if (fd < FIRST_FD) {
        unsigned long r = __syscall3(SYS_FD_WRITE, (unsigned long)fd, (unsigned long)buf, n);
        if (r == QUARK_ERR) {
            errno = EIO;
            return -1;
        }
        return (ssize_t)r;
    }

    struct openfile *f = slot(fd);
    if (!f) {
        errno = EBADF;
        return -1;
    }
    size_t done = 0;
    const unsigned char *in = buf;
    while (done < n) {
        size_t want = n - done;
        if (want > PAGE_SIZE) {
            want = PAGE_SIZE;
        }
        unsigned long put = 0;
        int err = quark_vfs_write(f->handle, in + done, f->offset, want, &put);
        if (err) {
            errno = vfs_errno(err);
            return done ? (ssize_t)done : -1;
        }
        done += put;
        f->offset += put;
        if (put < want) {
            break;
        }
    }
    return (ssize_t)done;
}

/* The program's process id, which is never used twice. Not SYS_GETPID: that
   answers with the task's id — what the kernel's own calls take, and what the
   next task made is given once this one has gone. */
pid_t getpid(void) {
    return (pid_t)__syscall1(SYS_PID, 0);
}

/* Sleep for `ns` nanoseconds: a receive from this task itself, which nobody
   sends to, so that only the time ends it. Gone back to if it ends early —
   a signal some other thread took. */
static void sleep_ns(unsigned long ns) {
    unsigned long self = __syscall0(SYS_GETPID);
    unsigned long until = quark_now() + ns;
    for (unsigned long now = quark_now(); now < until; now = quark_now()) {
        struct quark_msg m;
        __syscall3(SYS_RECV_TIMEOUT, self, (unsigned long)&m, quark_span(until - now));
    }
}

int usleep(unsigned int usec) {
    sleep_ns((unsigned long)usec * 1000UL);
    return 0;
}

unsigned int sleep(unsigned int seconds) {
    for (unsigned int i = 0; i < seconds; i++) {
        usleep(1000000);
    }
    return 0;
}

long readfile(const char *path, char *buf, long size) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = read(fd, buf, (size_t)size);
    close(fd);
    return n;
}

time_t time(time_t *t) {
    unsigned long wall = __syscall1(SYS_CLOCK, QUARK_CLOCK_WALL);
    time_t now = (time_t)((wall ? wall : quark_now()) / 1000000000UL);
    if (t) {
        *t = now;
    }
    return now;
}

clock_t clock(void) {
    return (clock_t)__syscall0(SYS_TICKS);
}

int nanosleep(const struct timespec *req, struct timespec *rem) {
    (void)rem;
    if (!req) {
        errno = EINVAL;
        return -1;
    }
    sleep_ns(quark_nanos((unsigned long)req->tv_sec, (unsigned long)req->tv_nsec));
    return 0;
}
