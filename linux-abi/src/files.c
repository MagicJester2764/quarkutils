/* Files, for a libc that thinks it is talking to Linux.
 *
 * A file on Quark is a handle held by the VFS, and its bytes and paths travel
 * in buffers lent to the server with each call. Linux's file calls are
 * descriptors, offsets and a `struct stat`. This is where one becomes the
 * other.
 *
 * A descriptor is the kernel's, whatever it names. A file is opened *as* one
 * (`OPEN_DESCRIPTOR`): the server puts a descriptor for its handle in this
 * program's table, and from then on the number is like any other — `dup2`
 * puts it where standard output was, a forked child has a copy, and the
 * program this one execs keeps it. The server keeps the position, which is
 * shared by every descriptor made from the first.
 *
 * So this file keeps almost nothing. It used to keep the table itself: an
 * array of open files in this program's memory, under numbers it made up
 * above the kernel's. `exec` threw that memory away, a forked child's copy
 * named handles the server would not answer for, and a file could not be put
 * on descriptor 1 because 1 was the kernel's and a file was not. What is left
 * is a note of which numbers are files and what the server calls them, kept
 * so that every read does not have to ask — and it is a note, not the truth:
 * anything that changes what a number names forgets it.
 */

#include <quark/layout.h>
#include <quark/syscall.h>
#include <quark/vfs.h>

#include "abi.h"

#define PAGE_SIZE 4096UL

/* Which descriptors are files. A descriptor is `UNKNOWN` until it is asked
   about, and again whenever it is closed or something is put on it. */
#define UNKNOWN 0
#define KERNELS 1 /* a pipe, a terminal, a stream, memory — or nothing */
#define A_FILE  2

static unsigned char kind[MAX_FDS];
static unsigned short handle_of[MAX_FDS];

/* Whether this program has ever taken a lock of its own (fcntl's F_SETLK),
   which is when closing a descriptor has something to drop. */
static int posix_locks_taken;

/* Linux's open flags, which are what musl passes. */
#define LX_O_ACCMODE   3
#define LX_O_WRONLY    1
#define LX_O_CREAT     0100
#define LX_O_EXCL      0200
#define LX_O_NOCTTY    0400
#define LX_O_TRUNC     01000
#define LX_O_APPEND    02000
#define LX_O_DIRECTORY 0200000
#define LX_O_NOFOLLOW  0400000
#define LX_O_CLOEXEC_  02000000
#define LX_O_PATH      010000000
#define LX_O_TMPFILE   020000000

#define LX_SEEK_SET 0
#define LX_SEEK_CUR 1
#define LX_SEEK_END 2

/* The server's codes are its own. This is where they become Linux's. */
static long vfs_errno(int code) {
    switch (code) {
    case QUARK_VFS_NOT_FOUND:      return -LX_ENOENT;
    case QUARK_VFS_INVALID_HANDLE: return -LX_EBADF;
    case QUARK_VFS_IO:             return -LX_EIO;
    case QUARK_VFS_TOO_MANY_OPEN:  return -LX_EMFILE;
    case QUARK_VFS_INVALID_PATH:   return -LX_EINVAL;
    case QUARK_VFS_NOT_DIR:        return -LX_ENOTDIR;
    case QUARK_VFS_IS_DIR:         return -LX_EISDIR;
    case QUARK_VFS_PERMISSION:     return -LX_EACCES;
    case QUARK_VFS_READ_ONLY:      return -LX_EROFS;
    case QUARK_VFS_UNREACHABLE:    return -LX_EIO;
    case QUARK_VFS_EXISTS:         return -LX_EEXIST;
    case QUARK_VFS_NOT_EMPTY:      return -LX_ENOTEMPTY;
    case QUARK_VFS_NOT_SUPPORTED:  return -LX_EOPNOTSUPP;
    case QUARK_VFS_NAME_TOO_LONG:  return -LX_ENAMETOOLONG;
    case QUARK_VFS_NO_SPACE:       return -LX_ENOSPC;
    case QUARK_VFS_TOO_MANY_LINKS: return -LX_EMLINK;
    case QUARK_VFS_LOOP:           return -LX_ELOOP;
    case QUARK_VFS_NO_PEER:        return -LX_ENXIO;
    case QUARK_VFS_BUSY:           return -LX_EBUSY;
    case QUARK_VFS_CROSS_DEVICE:   return -LX_EXDEV;
    default:                       return -LX_EIO;
    }
}

static void bytes_zero(void *p, unsigned long n) {
    unsigned char *b = p;
    while (n--) {
        *b++ = 0;
    }
}

static int streq(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

/* Something has changed what `fd` names, or is about to. */
void __quark_fd_forget(long fd) {
    if (fd >= 0 && fd < MAX_FDS) {
        kind[fd] = UNKNOWN;
    }
}

/* Whether `fd` is a file, and the server's handle for it if so. */
static int is_file(long fd, unsigned long *handle) {
    if (fd < 0 || fd >= MAX_FDS) {
        return 0;
    }
    if (kind[fd] == UNKNOWN) {
        unsigned long h;
        if (quark_vfs_handle(fd, &h) == 0) {
            handle_of[fd] = (unsigned short)h;
            kind[fd] = A_FILE;
        } else {
            kind[fd] = KERNELS;
        }
    }
    if (kind[fd] != A_FILE) {
        return 0;
    }
    if (handle) {
        *handle = handle_of[fd];
    }
    return 1;
}

int __quark_fd_is_file(long fd) {
    return is_file(fd, 0);
}

/* Whether `fd` names anything at all. The kernel says: its flags can be read
   only of a descriptor that is there. */
static int is_open(long fd) {
    return fd >= 0 && fd < MAX_FDS &&
           __syscall3(SYS_FD_FLAGS, (unsigned long)fd, QUARK_FD_GETFLAGS, 0) != QUARK_ERR;
}

/* The answer for a descriptor that is not a file: `not_a_file` if it is
   something else, EBADF if it is nothing. */
static long not_file(long fd, long not_a_file) {
    return is_open(fd) ? not_a_file : -LX_EBADF;
}

/* Where a relative path given with `dirfd` starts, said the way the server
   wants it: 0 for the working directory, a directory's handle plus one. An
   absolute path ignores the descriptor, as Linux does. */
static long base_for(long dirfd, const char *path, unsigned long *base) {
    *base = 0;
    if (dirfd == LX_AT_FDCWD || (path && path[0] == '/')) {
        return 0;
    }
    unsigned long h;
    if (!is_file(dirfd, &h)) {
        /* A pipe is real but no directory; anything else is no descriptor. */
        return not_file(dirfd, -LX_ENOTDIR);
    }
    /* Whether it is a directory is the server's to say, and it does. */
    *base = h + 1;
    return 0;
}

/* The permission bits a program leaves off what it makes. The kernel keeps
   them, because they have to survive what this memory does not. */
static unsigned long with_umask(long mode) {
    unsigned long mask = __syscall1(SYS_UMASK, ~0UL);
    return QUARK_VFS_MODE_GIVEN | ((unsigned long)mode & 07777 & ~mask);
}

long __quark_umask(long mask) {
    return (long)__syscall1(SYS_UMASK, (unsigned long)mask & 0777);
}

/* The names under /dev that are a descriptor this program already has. */
static long dev_alias(const char *path) {
    static const char *const std[3] = {"/dev/stdin", "/dev/stdout", "/dev/stderr"};
    for (long i = 0; i < 3; i++) {
        if (streq(path, std[i])) {
            return i;
        }
    }
    const char *p = path;
    for (const char *w = "/dev/fd/"; *w; w++, p++) {
        if (*p != *w) {
            return -1;
        }
    }
    if (!*p) {
        return -1;
    }
    long n = 0;
    for (; *p; p++) {
        if (*p < '0' || *p > '9' || n >= MAX_FDS) {
            return -1;
        }
        n = n * 10 + (*p - '0');
    }
    return n;
}

long __quark_open(const char *path, long flags, long mode) {
    return __quark_openat(LX_AT_FDCWD, path, flags, mode);
}

long __quark_openat(long dirfd, const char *path, long flags, long mode) {
    if (!path || !*path) {
        return -LX_ENOENT;
    }
    /* A terminal is not a file. `/dev/ptmx` and `/dev/pts/N` are pairs of
       kernel descriptors, caught here ahead of the VFS the way a Linux kernel
       catches them ahead of its filesystems. */
    if (__quark_pty_path(path)) {
        long tty = __quark_pty_open(path);
        if (tty >= 0) {
            __quark_pty_opened(tty, (flags & LX_O_NOCTTY) != 0);
        }
        return tty;
    }
    /* Nor is a name for a descriptor this program already holds: opening one
       is making another descriptor for the same thing. `/dev/tty` is whichever
       of the standard three is a terminal. */
    long alias = dev_alias(path);
    if (alias < 0 && streq(path, "/dev/tty")) {
        for (long fd = 0; fd < 3 && alias < 0; fd++) {
            if (__quark_ioctl(fd, 0x5401 /* TCGETS */, (unsigned long)(char[64]){0}) == 0) {
                alias = fd;
            }
        }
        if (alias < 0) {
            return -LX_ENXIO;
        }
    }
    if (alias >= 0) {
        long copy = __quark_dup(alias, -1);
        if (copy >= 0 && (flags & LX_O_CLOEXEC_)) {
            __syscall3(SYS_FD_FLAGS, (unsigned long)copy, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
        }
        return copy;
    }
    /* A file with no name is not something this filesystem can make. */
    if ((flags & LX_O_TMPFILE) == LX_O_TMPFILE) {
        return -LX_EOPNOTSUPP;
    }
    unsigned long base;
    long bad = base_for(dirfd, path, &base);
    if (bad) {
        return bad;
    }

    /* What the descriptor is for. O_PATH is for neither: it names the file,
       to be searched from or asked about, and needs no permission to read. */
    unsigned long how = 0;
    if (!(flags & LX_O_PATH)) {
        switch (flags & LX_O_ACCMODE) {
        case 0:           how |= QUARK_VFS_OPEN_READ; break;
        case LX_O_WRONLY: how |= QUARK_VFS_OPEN_WRITE; break;
        default:          how |= QUARK_VFS_OPEN_READ | QUARK_VFS_OPEN_WRITE; break;
        }
    }
    if (flags & LX_O_CREAT) {
        how |= QUARK_VFS_OPEN_CREATE;
        if (flags & LX_O_EXCL) {
            how |= QUARK_VFS_OPEN_EXCLUSIVE;
        }
    }
    if (flags & LX_O_DIRECTORY) {
        how |= QUARK_VFS_OPEN_DIRECTORY;
    }
    if ((flags & LX_O_TRUNC) && (how & QUARK_VFS_OPEN_WRITE)) {
        how |= QUARK_VFS_OPEN_TRUNCATE;
    }
    if (flags & LX_O_NOFOLLOW) {
        how |= QUARK_VFS_OPEN_NOFOLLOW;
    }
    if (flags & LX_O_APPEND) {
        how |= QUARK_VFS_OPEN_APPEND;
    }
    if (flags & LX_O_NONBLOCK) {
        how |= QUARK_VFS_OPEN_NOWAIT;
    }
    struct quark_vfs_file info;
    long fd = -1;
    int err = quark_vfs_open_fd(base, path, how,
                                (flags & LX_O_CREAT) ? with_umask(mode) : 0, &info, &fd);
    if (err) {
        return vfs_errno(err);
    }
    __quark_fd_forget(fd);
    /* What O_NOFOLLOW found a link at is not opened: that is its point.
       O_PATH with it is how a link itself is named, and that is allowed. */
    if ((info.mode & 0170000) == 0120000 && !(flags & LX_O_PATH)) {
        __syscall1(SYS_FD_CLOSE, (unsigned long)fd);
        return -LX_ELOOP;
    }
    if ((info.mode & 0170000) == 0010000 && (how & (QUARK_VFS_OPEN_READ | QUARK_VFS_OPEN_WRITE))) {
        /* A named pipe: what came back is an end of a pipe, and the kernel's
           from here on. Opening one waits for somebody to open the other
           end, unless it was asked not to — a reader that went ahead would
           find no writer, which is how a pipe says it has ended. `info.size`
           is what to wait on, and nothing if the other end is there. */
        if (fd >= 0 && fd < MAX_FDS) {
            kind[fd] = KERNELS;
        }
        while (info.size && !(flags & LX_O_NONBLOCK)) {
            unsigned long r = __syscall2(SYS_PIPE_PEER, (unsigned long)fd, info.size);
            if (r == 0) {
                break;
            }
            /* A signal ended the wait. The open is made again from where it
               was if the handler asked for that, and is over if it did not:
               the end goes back, so that nobody is left waiting for a
               program that has stopped opening it. */
            if (r != QUARK_INTERRUPTED || (__quark_sig_interrupted() & QUARK_SIG_EINTR)) {
                __syscall1(SYS_FD_CLOSE, (unsigned long)fd);
                return r == QUARK_INTERRUPTED ? -LX_EINTR : -LX_EIO;
            }
        }
        __quark_fd_set_nonblock(fd, (flags & LX_O_NONBLOCK) != 0);
    } else if (fd >= 0 && fd < MAX_FDS) {
        handle_of[fd] = (unsigned short)info.handle;
        kind[fd] = A_FILE;
    }
    if (flags & LX_O_CLOEXEC_) {
        __syscall3(SYS_FD_FLAGS, (unsigned long)fd, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
    }
    return fd;
}

/* mknod, mknodat and so mkfifo. A named pipe is the one thing there is to
   make: a device is not a file anybody can create here, and the type bits
   left at zero mean a regular file, which is made by opening it. */
long __quark_mknodat(long dirfd, const char *path, long mode) {
    if (!path || !*path) {
        return -LX_ENOENT;
    }
    switch (mode & 0170000) {
    case 0010000:
        break;
    case 0:
    case 0100000: {
        long fd = __quark_openat(dirfd, path, LX_O_CREAT | LX_O_EXCL | LX_O_WRONLY, mode & 07777);
        if (fd < 0) {
            return fd;
        }
        __quark_close(fd);
        return 0;
    }
    default:
        return -LX_EPERM;
    }
    unsigned long base;
    long bad = base_for(dirfd, path, &base);
    if (bad) {
        return bad;
    }
    unsigned long mask = __syscall1(SYS_UMASK, ~0UL);
    int err = quark_vfs_mknod(base, path, 0010000 | ((unsigned long)mode & 07777 & ~mask));
    /* A filesystem with no such thing — FAT — says what Linux's says. */
    return err == QUARK_VFS_NOT_SUPPORTED ? -LX_EPERM : err ? vfs_errno(err) : 0;
}

long __quark_close(long fd) {
    unsigned long h;
    if (posix_locks_taken && is_file(fd, &h)) {
        /* Closing any descriptor for a file drops the program's locks on it,
           even one whose open file lives on in a copy. The server hears only
           of the last close, so this one is said as an unlock. */
        quark_vfs_lock(h, 0, 0, 0, 0, 0);
    }
    __quark_fd_forget(fd);
    if (fd >= 0 && fd < MAX_FDS && __syscall1(SYS_FD_CLOSE, (unsigned long)fd) != QUARK_ERR) {
        return 0;
    }
    /* 0, 1 and 2 belong to whoever started this program, and one that was
       never wired up is finished with all the same. Reporting EBADF makes
       every tool that tidies up after itself print an error it cannot act
       on. */
    return (fd >= 0 && fd < 3) ? 0 : -LX_EBADF;
}

/* dup, dup2 and dup3: `to` is the number wanted, or negative for the lowest
   free. Every descriptor is the kernel's, so this is the kernel's whatever
   `fd` names — which is what lets a file go where standard output was. */
long __quark_dup(long fd, long to) {
    if (fd < 0 || fd >= MAX_FDS || to >= MAX_FDS) {
        return -LX_EBADF;
    }
    unsigned long me = __syscall0(SYS_GETPID);
    unsigned long r;
    if (to < 0) {
        r = __syscall4(SYS_FD_DUP, me, QUARK_ANY_FD, (unsigned long)fd, 0);
        if (r == QUARK_ERR) {
            return is_open(fd) ? -LX_EMFILE : -LX_EBADF;
        }
    } else {
        /* Onto a number: what was there is closed, and a copy onto itself
           changes nothing. Both are the kernel's to do, in one step. */
        r = __syscall4(SYS_FD_DUP, me, (unsigned long)to, (unsigned long)fd, 0);
        if (r == QUARK_ERR) {
            return -LX_EBADF;
        }
    }
    __quark_fd_forget((long)r);
    /* Whether a descriptor waits is this layer's to remember, and a copy of
       one that does not wait does not either. */
    if ((long)r != fd) {
        __quark_fd_set_nonblock((long)r, __quark_fd_is_nonblock(fd));
    }
    return (long)r;
}

/* Read or write through a file's handle, a page a message. `at` is an offset,
   or QUARK_VFS_AT_POSITION for wherever the descriptor is: the server then
   moves it, a piece at a time, so that nothing here has to. */
static long file_read(unsigned long h, void *buf, unsigned long n, unsigned long at) {
    unsigned long done = 0;
    unsigned char *out = buf;
    while (done < n) {
        unsigned long want = n - done;
        if (want > PAGE_SIZE) {
            want = PAGE_SIZE;
        }
        unsigned long got = 0;
        unsigned long where = at == QUARK_VFS_AT_POSITION ? at : at + done;
        /* The caller's own buffer, lent to the VFS to fill. */
        int e = quark_vfs_read(h, out + done, where, want, &got);
        if (e) {
            return done ? (long)done : vfs_errno(e);
        }
        done += got;
        /* A short read means the end, not a hiccup: the server answers from a
           page at a time and gives everything it has. */
        if (got < want) {
            break;
        }
    }
    return (long)done;
}

static long file_write(unsigned long h, const void *buf, unsigned long n, unsigned long at) {
    unsigned long done = 0;
    const unsigned char *in = buf;
    while (done < n) {
        unsigned long want = n - done;
        if (want > PAGE_SIZE) {
            want = PAGE_SIZE;
        }
        unsigned long put = 0;
        unsigned long where = at == QUARK_VFS_AT_POSITION ? at : at + done;
        int e = quark_vfs_write(h, in + done, where, want, &put);
        if (e) {
            return done ? (long)done : vfs_errno(e);
        }
        done += put;
        if (put < want) {
            break;
        }
    }
    return (long)done;
}

long __quark_pread(long fd, void *buf, unsigned long n, long offset) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_ESPIPE);
    }
    if (offset < 0) {
        return -LX_EINVAL;
    }
    return n ? file_read(h, buf, n, (unsigned long)offset) : 0;
}

long __quark_pwrite(long fd, const void *buf, unsigned long n, long offset) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_ESPIPE);
    }
    if (offset < 0) {
        return -LX_EINVAL;
    }
    return n ? file_write(h, buf, n, (unsigned long)offset) : 0;
}

long __quark_lseek(long fd, long offset, long whence) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        /* A pipe or a terminal, and seeking one is the error Linux calls
           ESPIPE — which is what stdio checks for when it decides whether a
           stream is seekable. */
        return not_file(fd, -LX_ESPIPE);
    }
    /* SEEK_DATA and SEEK_HOLE are not offered: a caller that asks is told
       so, and copies the file the long way. */
    if (whence < LX_SEEK_SET || whence > LX_SEEK_END) {
        return -LX_EINVAL;
    }
    unsigned long pos;
    int err = quark_vfs_seek(h, offset, (unsigned long)whence, &pos, 0);
    if (err) {
        return err == QUARK_VFS_INVALID_PATH ? -LX_EINVAL : vfs_errno(err);
    }
    return (long)pos;
}

/* The kernel's `struct stat` for x86-64, which is what musl copies out of.
   Its layout is the ABI, not musl's choice, so it is spelled out here. */
struct lx_kstat {
    unsigned long st_dev;
    unsigned long st_ino;
    unsigned long st_nlink;
    unsigned int  st_mode;
    unsigned int  st_uid;
    unsigned int  st_gid;
    unsigned int  __pad0;
    unsigned long st_rdev;
    long          st_size;
    long          st_blksize;
    long          st_blocks;
    long          st_atime_sec, st_atime_nsec;
    long          st_mtime_sec, st_mtime_nsec;
    long          st_ctime_sec, st_ctime_nsec;
    long          __unused[3];
};

static void fill_stat(struct lx_kstat *st, const struct quark_vfs_stat *r) {
    bytes_zero(st, sizeof *st);
    /* A file in a mounted filesystem says so above the fortieth bit of its
       id: which mount, and which inside that. Its device is not the root's,
       and its number is the one its own filesystem gave it. */
    unsigned long id = r->id & QUARK_VFS_ID_MASK;
    st->st_dev = 1 + (r->id >> 40);
    st->st_ino = id;
    st->st_nlink = r->links;
    st->st_mode = (unsigned int)r->mode;
    st->st_uid = (unsigned int)r->uid;
    st->st_gid = (unsigned int)r->gid;
    st->st_size = (long)r->size;
    st->st_blksize = (long)r->blksize;
    st->st_blocks = (long)r->blocks;
    st->st_atime_sec = (long)r->atime;
    st->st_mtime_sec = (long)r->mtime;
    st->st_ctime_sec = (long)r->ctime;
    /* The server's devices, by the numbers Linux gives them: 1:3 null,
       1:5 zero, 1:7 full, 1:8 random, 1:9 urandom. */
    static const unsigned char minors[] = {3, 5, 7, 8, 9};
    unsigned long dev = id - QUARK_VFS_DEVICE_ID;
    if ((r->mode & 0170000) == 020000 && dev < sizeof minors) {
        st->st_rdev = (1ul << 8) | minors[dev];
    }
    /* A disk. Linux gives a block device no size here — a program asks the
       device (BLKGETSIZE64), or seeks to its end — and a number: 8 for a
       disk and 1 for one made of memory, the driver and the partition in
       the minor. The server numbers them thirty-two to a driver, disks
       first. */
    if ((r->mode & 0170000) == 060000) {
        unsigned long n = id - QUARK_VFS_BLOCK_ID;
        st->st_size = 0;
        st->st_rdev = n < 4 * 32 ? (8ul << 8) | n : (1ul << 8) | (n - 4 * 32);
    }
}

/* What a program asks a disk: how big it is, how big its sectors are, and to
   look at its partition table again. Not a disk, and the answer is ENOTTY,
   as it is to any question a descriptor has no answer to. */
#define LX_BLKROGET     0x125EUL
#define LX_BLKRRPART    0x125FUL
#define LX_BLKGETSIZE   0x1260UL
#define LX_BLKFLSBUF    0x1261UL
#define LX_BLKSSZGET    0x1268UL
#define LX_BLKBSZGET    0x80081270UL
#define LX_BLKGETSIZE64 0x80081272UL
#define LX_BLKIOMIN     0x1278UL
#define LX_BLKIOOPT     0x1279UL
#define LX_BLKALIGNOFF  0x127AUL
#define LX_BLKPBSZGET   0x127BUL

long __quark_blk_ioctl(long fd, unsigned long request, unsigned long arg) {
    unsigned long h;
    struct quark_vfs_stat st;
    if (!is_file(fd, &h) || quark_vfs_stat(h, &st) || (st.mode & 0170000) != 060000) {
        return -LX_ENOTTY;
    }
    switch (request) {
    case LX_BLKGETSIZE64:
        *(unsigned long *)arg = st.size;
        return 0;
    case LX_BLKGETSIZE:
        *(unsigned long *)arg = st.size / 512;
        return 0;
    case LX_BLKBSZGET:
        *(unsigned long *)arg = 512;
        return 0;
    case LX_BLKSSZGET:
    case LX_BLKPBSZGET:
    case LX_BLKIOMIN:
        *(unsigned int *)arg = 512;
        return 0;
    case LX_BLKIOOPT:
    case LX_BLKALIGNOFF:
    case LX_BLKROGET:
        *(unsigned int *)arg = 0;
        return 0;
    case LX_BLKFLSBUF:
        /* Nothing is kept back to flush: a write is on the disk when it
           returns. */
        return 0;
    case LX_BLKRRPART: {
        int err = quark_vfs_devctl(h, QUARK_VFS_DEVCTL_RESCAN);
        return err ? vfs_errno(err) : 0;
    }
    default:
        return -LX_ENOTTY;
    }
}

/* The device the terminals under /dev/pts are on: not the filesystem's, and
   not the one the kernel's other descriptors are given. */
#define PTS_DEV 24

long __quark_fstat(long fd, void *statbuf) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        if (!is_open(fd) && !(fd >= 0 && fd < 3)) {
            return -LX_EBADF;
        }
        /* Not a file: one of the kernel's own, which says what kind. A
           program asks in order to choose — whether to seek, how to buffer,
           whether two descriptors are the same thing — so a pipe has to be a
           pipe and a terminal a terminal. Its device is not the filesystem's,
           so that nothing mistakes it for the file whose inode has its
           number. */
        struct lx_kstat *st = statbuf;
        bytes_zero(st, sizeof *st);
        st->st_dev = 2;
        st->st_ino = (unsigned long)fd;
        st->st_nlink = 1;
        st->st_blksize = (long)PAGE_SIZE;
        unsigned long k = __syscall1(SYS_FD_KIND, (unsigned long)fd);
        switch (k == QUARK_ERR ? 0 : QUARK_FD_KIND(k)) {
        case QUARK_FD_KIND_PIPE_READ:
        case QUARK_FD_KIND_PIPE_WRITE:
            st->st_mode = 010000 | 0600; /* S_IFIFO */
            break;
        case QUARK_FD_KIND_STREAM:
        case QUARK_FD_KIND_SOCKET:
            st->st_mode = 0140000 | 0777; /* S_IFSOCK */
            break;
        case QUARK_FD_KIND_PTY_SLAVE: {
            /* /dev/pts/N: major 136, and the pty's number. One terminal is
               one file however many descriptors a program has for it, so its
               inode is the terminal's and not the descriptor's — which is
               also what lets `ttyname` check the name it was given against
               the descriptor it asked about. */
            unsigned long number = __syscall3(SYS_PTY_CTL, (unsigned long)fd, 4, 0);
            if (number == QUARK_ERR) {
                number = 0;
            }
            st->st_mode = 020000 | 0620; /* S_IFCHR */
            st->st_rdev = (136ul << 8) | number;
            st->st_dev = PTS_DEV;
            st->st_ino = number + 3;
            break;
        }
        case QUARK_FD_KIND_PTY_MASTER:
            st->st_mode = 020000 | 0666;
            st->st_rdev = (5ul << 8) | 2; /* /dev/ptmx */
            break;
        case QUARK_FD_KIND_MEMORY:
            st->st_mode = 0100000 | 0777; /* S_IFREG, as a memfd is */
            break;
        case QUARK_FD_KIND_TIMER:
        case QUARK_FD_KIND_EVENT:
        case QUARK_FD_KIND_POLLSET:
            st->st_mode = 0600; /* no type at all, which is what Linux says */
            break;
        default:
            /* An endpoint — a service on the other end of a descriptor, which
               is what standard input is on a console that is no terminal. */
            st->st_mode = 020000 | 0666;
            break;
        }
        return 0;
    }
    /* Asked, not remembered: another descriptor, or another program, may
       have changed the file since this one was opened. */
    struct quark_vfs_stat r;
    int err = quark_vfs_stat(h, &r);
    if (err) {
        return vfs_errno(err);
    }
    fill_stat(statbuf, &r);
    return 0;
}

long __quark_stat(long dirfd, const char *path, void *statbuf, int follow) {
    if (!path || !*path) {
        return -LX_ENOENT;
    }
    long alias = dev_alias(path);
    if (alias < 0) {
        /* A terminal this program has open, by its name. */
        alias = __quark_pty_held(path);
    }
    if (alias >= 0) {
        return __quark_fstat(alias, statbuf);
    }
    unsigned long base;
    long bad = base_for(dirfd, path, &base);
    if (bad) {
        return bad;
    }
    /* A handle of this program's own, for as long as the question takes. */
    struct quark_vfs_file info;
    int err = quark_vfs_open_at(base, path, follow ? 0 : QUARK_VFS_OPEN_NOFOLLOW, &info);
    if (err) {
        return vfs_errno(err);
    }
    struct quark_vfs_stat r;
    err = quark_vfs_stat(info.handle, &r);
    quark_vfs_close(info.handle);
    if (err) {
        return vfs_errno(err);
    }
    fill_stat(statbuf, &r);
    return 0;
}

static unsigned long rd(const unsigned char *p, int n) {
    unsigned long v = 0;
    for (int i = n - 1; i >= 0; i--) {
        v = (v << 8) | p[i];
    }
    return v;
}

static void wr(unsigned char *p, int n, unsigned long v) {
    for (int i = 0; i < n; i++) {
        p[i] = (unsigned char)(v >> (8 * i));
    }
}

/* getdents64: the server's records, copied field by field into Linux's
   (d_ino, d_off, d_reclen, d_type, d_name). A Linux record is always the
   shorter of the two, so what fits a page of the server's fits the caller's
   buffer of the same size. The directory's position is the server's. */
long __quark_getdents(long fd, void *buf, unsigned long count) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_ENOTDIR);
    }
    unsigned char page[4096];
    unsigned long want = count < sizeof page ? count : sizeof page;
    unsigned long used = 0, next = 0;
    int end = 0;
    int e = quark_vfs_readdir(h, QUARK_VFS_AT_POSITION, page, want, &used, &next, &end);
    if (e) {
        return vfs_errno(e);
    }
    unsigned char *out = buf;
    unsigned long in = 0, put = 0, resume = 0;
    int short_of = 0;
    while (in + QUARK_VFS_DIRENT_HEADER <= used) {
        const unsigned char *r = page + in;
        unsigned long reclen = rd(r + 24, 2);
        unsigned long namelen = r[27];
        unsigned long lreclen = (19 + namelen + 1 + 7) & ~7UL;
        if (reclen < QUARK_VFS_DIRENT_HEADER + namelen || in + reclen > used) {
            return put ? (long)put : -LX_EIO;
        }
        if (put + lreclen > count) {
            short_of = 1;
            break;
        }
        bytes_zero(out + put, lreclen);
        wr(out + put, 8, rd(r, 8) & QUARK_VFS_ID_MASK); /* d_ino, as stat says it */
        wr(out + put + 8, 8, rd(r + 8, 8));   /* d_off: where to resume after it */
        wr(out + put + 16, 2, lreclen);       /* d_reclen */
        out[put + 18] = r[26];                /* d_type */
        for (unsigned long i = 0; i < namelen; i++) {
            out[put + 19 + i] = r[QUARK_VFS_DIRENT_HEADER + i];
        }
        resume = rd(r + 8, 8);
        put += lreclen;
        in += reclen;
    }
    if (short_of) {
        /* The server moved past records the caller had no room for: put the
           directory back after the last one it was given. */
        quark_vfs_seek(h, (long)resume, QUARK_VFS_SEEK_SET, 0, 0);
    }
    if (put == 0 && used != 0) {
        return -LX_EINVAL; /* the caller's buffer holds no entry */
    }
    return (long)put;
}

/* "/proc/self/fd/7" -> 7, or -1 for anything else. */
static long proc_fd(const char *path) {
    const char *p = path;
    for (const char *w = "/proc/self/fd/"; *w; w++, p++) {
        if (*p != *w) {
            return -1;
        }
    }
    if (!*p) {
        return -1;
    }
    long n = 0;
    for (; *p; p++) {
        if (*p < '0' || *p > '9' || n >= MAX_FDS) {
            return -1;
        }
        n = n * 10 + (*p - '0');
    }
    return n < MAX_FDS ? n : -1;
}

/* readlink: at most `size` bytes of the target, and no NUL. */
long __quark_readlink(long dirfd, const char *path, char *buf, unsigned long size) {
    if ((long)size <= 0) {
        return -LX_EINVAL;
    }
    if (!path || !*path) {
        return -LX_ENOENT;
    }
    /* There is no /proc. But `/proc/self/fd/N` is how a C library asks what
       a descriptor is called — it is the whole of musl's `ttyname` — and a
       terminal has a name to give: the one it is opened by. */
    long fd = proc_fd(path);
    if (fd >= 0) {
        long number = __quark_pty_slave_number(fd);
        if (number < 0) {
            return -LX_ENOENT;
        }
        char name[24] = "/dev/pts/";
        unsigned long len = 9;
        char digits[8];
        int n = 0;
        do {
            digits[n++] = (char)('0' + number % 10);
            number /= 10;
        } while (number && n < 8);
        while (n) {
            name[len++] = digits[--n];
        }
        if (len > size) {
            len = size;
        }
        for (unsigned long i = 0; i < len; i++) {
            buf[i] = name[i];
        }
        return (long)len;
    }
    unsigned long base;
    long bad = base_for(dirfd, path, &base);
    if (bad) {
        return bad;
    }
    long len = quark_vfs_readlink_at(base, path, buf, size);
    if (len < 0) {
        return vfs_errno((int)-len);
    }
    return (unsigned long)len < size ? len : (long)size;
}

long __quark_symlink(const char *target, long dirfd, const char *path) {
    if (!target || !*target || !path || !*path) {
        return -LX_ENOENT;
    }
    unsigned long base;
    long bad = base_for(dirfd, path, &base);
    if (bad) {
        return bad;
    }
    int err = quark_vfs_symlink_at(target, base, path);
    /* FAT32 has no links: Linux says EPERM for a filesystem without them. */
    if (err == QUARK_VFS_NOT_SUPPORTED) {
        return -LX_EPERM;
    }
    return err ? vfs_errno(err) : 0;
}

/* fsync, fdatasync and syncfs: an open descriptor, and then everything.
   sync is the same with no descriptor to ask about (-1), and cannot fail. */
long __quark_fsync(long fd) {
    if (fd == -1) {
        quark_vfs_sync();
        return 0;
    }
    if (!is_file(fd, 0)) {
        /* A pipe, a terminal, a socket: open, and not something a
           filesystem records. */
        return is_open(fd) || (fd >= 0 && fd < 3) ? -LX_EINVAL : -LX_EBADF;
    }
    return quark_vfs_sync() ? -LX_EIO : 0;
}

/* Linux's struct statfs for x86-64: seven words, a two-int fsid, four more
   words and four spare. */
static long fill_statfs(unsigned long handle, unsigned char *out) {
    struct quark_vfs_statfs fs;
    int err = quark_vfs_statfs_of(handle, &fs);
    if (err) {
        return vfs_errno(err);
    }
    bytes_zero(out, 120);
    wr(out + 0, 8, fs.magic);
    wr(out + 8, 8, fs.bsize);
    wr(out + 16, 8, fs.blocks);
    wr(out + 24, 8, fs.bfree);
    wr(out + 32, 8, fs.bavail);
    wr(out + 40, 8, fs.files);
    wr(out + 48, 8, fs.ffree);
    wr(out + 64, 8, fs.namemax);  /* f_namelen */
    wr(out + 72, 8, fs.bsize);    /* f_frsize */
    return 0;
}

/* The filesystem the path is in, which the file server knows from having
   opened it: a mounted one answers for itself. */
long __quark_statfs(const char *path, void *buf) {
    struct quark_vfs_file info;
    int err = quark_vfs_open(path, 0, &info);
    if (err) {
        return vfs_errno(err);
    }
    long done = fill_statfs(info.handle, buf);
    quark_vfs_close(info.handle);
    return done;
}

long __quark_fstatfs(long fd, void *buf) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_ENOSYS);
    }
    return fill_statfs(h, buf);
}

static int chdir_at(unsigned long base, const char *path) {
    (void)base; /* always the working directory's own: chdir has no dirfd */
    return quark_vfs_chdir(path);
}

/* A path request that answers only yes or no. */
static long path_request(long dirfd, const char *path,
                         int (*request)(unsigned long, const char *)) {
    if (!path || !*path) {
        return -LX_ENOENT;
    }
    unsigned long base;
    long bad = base_for(dirfd, path, &base);
    if (bad) {
        return bad;
    }
    int err = request(base, path);
    return err ? vfs_errno(err) : 0;
}

long __quark_mkdir(long dirfd, const char *path, long mode) {
    if (!path || !*path) {
        return -LX_ENOENT;
    }
    unsigned long base;
    long bad = base_for(dirfd, path, &base);
    if (bad) {
        return bad;
    }
    int err = quark_vfs_mkdir_mode(base, path, with_umask(mode));
    return err ? vfs_errno(err) : 0;
}

long __quark_unlink(long dirfd, const char *path) {
    return path_request(dirfd, path, quark_vfs_unlink_at);
}

long __quark_rmdir(long dirfd, const char *path) {
    return path_request(dirfd, path, quark_vfs_rmdir_at);
}

long __quark_rename(long fromfd, const char *from, long tofd, const char *to) {
    if (!from || !*from || !to || !*to) {
        return -LX_ENOENT;
    }
    unsigned long fbase, tbase;
    long bad = base_for(fromfd, from, &fbase);
    if (!bad) {
        bad = base_for(tofd, to, &tbase);
    }
    if (bad) {
        return bad;
    }
    int err = quark_vfs_rename_at(fbase, from, tbase, to);
    return err ? vfs_errno(err) : 0;
}

long __quark_chdir(const char *path) {
    return path_request(LX_AT_FDCWD, path, chdir_at);
}

long __quark_fchdir(long fd) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_ENOTDIR);
    }
    struct quark_vfs_stat r;
    int err = quark_vfs_stat(h, &r);
    if (err) {
        return vfs_errno(err);
    }
    if ((r.mode & 0170000) != 0040000) {
        return -LX_ENOTDIR;
    }
    /* Where a program is, is a descriptor: one past the ordinary numbers.
       Being in the directory this one names is a copy of it there. */
    unsigned long me = __syscall0(SYS_GETPID);
    return __syscall4(SYS_FD_DUP, me, QUARK_FD_CWD, (unsigned long)fd, 0) == QUARK_ERR
               ? -LX_EBADF
               : 0;
}

/* getcwd(2) returns the length with the NUL, and ERANGE when that does not
   fit; a directory that has been removed is ENOENT, as on Linux. */
long __quark_getcwd(char *buf, unsigned long size) {
    if (!buf || size == 0) {
        return -LX_ERANGE;
    }
    long n = quark_vfs_getcwd(buf, size - 1);
    if (n == -QUARK_VFS_NAME_TOO_LONG) {
        return -LX_ERANGE;
    }
    if (n < 0) {
        return vfs_errno((int)-n);
    }
    buf[n] = 0;
    return n + 1;
}

long __quark_link(long fromfd, const char *from, long tofd, const char *to, int follow) {
    if (!from || !*from || !to || !*to) {
        return -LX_ENOENT;
    }
    unsigned long fbase, tbase;
    long bad = base_for(fromfd, from, &fbase);
    if (!bad) {
        bad = base_for(tofd, to, &tbase);
    }
    if (bad) {
        return bad;
    }
    int err = quark_vfs_link_at(fbase, from, tbase, to, follow);
    /* A directory, and a filesystem with no hard links, are both EPERM on
       Linux, and EPERM is what fontconfig's lock knows to fall back from. */
    if (err == QUARK_VFS_IS_DIR || err == QUARK_VFS_NOT_SUPPORTED) {
        return -LX_EPERM;
    }
    return err ? vfs_errno(err) : 0;
}

/* ftruncate on a file. Memory named by a descriptor is net.c's. */
long __quark_file_truncate(long fd, long length) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_EINVAL);
    }
    if (length < 0) {
        return -LX_EINVAL;
    }
    int err = quark_vfs_truncate(h, (unsigned long)length);
    if (err) {
        /* A descriptor not open for writing is EINVAL to ftruncate. */
        return err == QUARK_VFS_INVALID_HANDLE ? -LX_EINVAL : vfs_errno(err);
    }
    return 0;
}

long __quark_truncate(const char *path, long length) {
    if (length < 0) {
        return -LX_EINVAL;
    }
    struct quark_vfs_file info;
    int err = quark_vfs_open(path, 0, &info);
    if (err) {
        return vfs_errno(err);
    }
    if (info.is_dir) {
        quark_vfs_close(info.handle);
        return -LX_EISDIR;
    }
    err = quark_vfs_truncate(info.handle, (unsigned long)length);
    quark_vfs_close(info.handle);
    return err ? vfs_errno(err) : 0;
}

/* access(2), and the *at forms of it that gnulib reaches for first.
 *
 * The question is "may I", and the only way to ask it is to open the file:
 * that runs the server's own permission check, and the reply says what this
 * caller may do with what it found. Working it out from the mode bits here
 * would need the file's owner as well, and would be this program's opinion
 * about a policy the VFS enforces — which is the one that decides.
 *
 * The one thing to know is that Quark checks nothing on execute: init and the
 * shell load a program by reading it. So a file the server calls executable is
 * one a program is allowed to try, which is what a caller asks X_OK for. */
long __quark_access(long dirfd, const char *path, long mode) {
    if (!path || !*path) {
        return -LX_ENOENT;
    }
    unsigned long base;
    long bad = base_for(dirfd, path, &base);
    if (bad) {
        return bad;
    }
    struct quark_vfs_file info;
    int err = quark_vfs_open_at(base, path, 0, &info);
    if (err) {
        return vfs_errno(err);
    }
    unsigned int have = info.access;
    quark_vfs_close(info.handle);

    /* F_OK is 0 — asking only whether it is there, which the open answered. */
    unsigned int want = (unsigned int)mode & 7;
    return (want & ~have) ? -LX_EACCES : 0;
}

/* A read or a write goes to the server for a file, where the descriptor's
   position is, and to the kernel for everything else. */
long __quark_read(long fd, void *buf, unsigned long n) {
    unsigned long h;
    if (is_file(fd, &h)) {
        return n ? file_read(h, buf, n, QUARK_VFS_AT_POSITION) : 0;
    }
    if (fd < 0 || fd >= MAX_FDS) {
        return -LX_EBADF;
    }
    /* O_NONBLOCK has to mean it. A main loop drains its wake-up "until it
       is empty", which is a read that answers EAGAIN rather than one that
       waits; glib's does exactly that with its context lock held, so a
       read that blocks there is a program that stops rather than one that
       fails. Setting the flag and ignoring it is the worst of both. */
    unsigned long r;
    if (__quark_fd_is_nonblock(fd)) {
        r = __syscall3(SYS_FD_READ_NB, (unsigned long)fd, (unsigned long)buf, n);
        if (r == QUARK_WOULD_BLOCK) {
            return -LX_EAGAIN;
        }
    } else {
        for (;;) {
            r = __syscall3(SYS_FD_READ, (unsigned long)fd, (unsigned long)buf, n);
            if (r != QUARK_INTERRUPTED) {
                break;
            }
            /* A read of a terminal that a signal ended. If a handler ran and
               did not ask for the read to go on, the read is over. */
            if (__quark_sig_interrupted() & QUARK_SIG_EINTR) {
                return -LX_EINTR;
            }
        }
    }
    if (r == QUARK_ERR) {
        /* The kernel says only that it failed. A terminal that would not be
           read is one this program is behind, and may not be stopped for:
           it ignores the signal, or has nobody who would start it again. */
        unsigned long k = __syscall1(SYS_FD_KIND, (unsigned long)fd);
        return k != QUARK_ERR && QUARK_FD_KIND(k) == QUARK_FD_KIND_PTY_SLAVE ? -LX_EIO : -LX_EBADF;
    }
    return (long)r;
}

/* Why a write to one of the kernel's descriptors failed. The kernel says
   only that it did; what the descriptor is, and whether anybody is left at
   the other end of it, says the rest. */
static long write_failed(long fd) {
    unsigned long k = __syscall1(SYS_FD_KIND, (unsigned long)fd);
    if (k == QUARK_ERR) {
        return -LX_EBADF;
    }
    switch (QUARK_FD_KIND(k)) {
    case QUARK_FD_KIND_PIPE_WRITE:
    case QUARK_FD_KIND_STREAM:
        if (k & QUARK_FD_GONE) {
            /* Nobody will read it. By default that is the end of this
               program, quietly, which is how `yes | head` ends. */
            __quark_sig_pipe();
            return -LX_EPIPE;
        }
        return -LX_EIO;
    case QUARK_FD_KIND_PTY_MASTER:
    case QUARK_FD_KIND_PTY_SLAVE:
    case QUARK_FD_KIND_ENDPOINT:
    case QUARK_FD_KIND_SOCKET:
        return -LX_EIO;
    default:
        /* Open, and not a thing that is written to. */
        return -LX_EBADF;
    }
}

long __quark_write(long fd, const void *buf, unsigned long n) {
    unsigned long h;
    if (is_file(fd, &h)) {
        return n ? file_write(h, buf, n, QUARK_VFS_AT_POSITION) : 0;
    }
    if (fd < 0 || fd >= MAX_FDS) {
        return -LX_EBADF;
    }
    unsigned long r;
    if (__quark_fd_is_nonblock(fd)) {
        r = __syscall3(SYS_FD_WRITE_NB, (unsigned long)fd, (unsigned long)buf, n);
        if (r == QUARK_WOULD_BLOCK) {
            return -LX_EAGAIN;
        }
    } else {
        r = __syscall3(SYS_FD_WRITE, (unsigned long)fd, (unsigned long)buf, n);
    }
    return r == QUARK_ERR ? write_failed(fd) : (long)r;
}

/* chmod, chown and the times, by path or by descriptor: one request, which
   says which of five words it means. */
static long set_attrs(long dirfd, const char *path, int nofollow, unsigned long which,
                      const unsigned long attrs[5]) {
    unsigned long base;
    if (path && *path) {
        long bad = base_for(dirfd, path, &base);
        if (bad) {
            return bad;
        }
    } else {
        /* No path: the open file itself. */
        unsigned long h;
        if (!is_file(dirfd, &h)) {
            /* A terminal's mode is not anybody's to change here; saying yes
               lets a program that tidies its descriptors carry on. */
            return not_file(dirfd, 0);
        }
        base = h + 1;
        path = 0;
    }
    int err = quark_vfs_setattr(base, path, which, nofollow, attrs);
    if (err == QUARK_VFS_PERMISSION) {
        return -LX_EPERM;
    }
    return err ? vfs_errno(err) : 0;
}

long __quark_chmod(long dirfd, const char *path, long mode, int nofollow) {
    unsigned long attrs[5] = {(unsigned long)mode & 07777, 0, 0, 0, 0};
    if (path && !*path) {
        return -LX_ENOENT;
    }
    return set_attrs(dirfd, path, nofollow, QUARK_VFS_ATTR_MODE, attrs);
}

long __quark_chown(long dirfd, const char *path, long uid, long gid, int nofollow) {
    unsigned long attrs[5] = {0, (unsigned long)uid, (unsigned long)gid, 0, 0};
    unsigned long which = 0;
    if (path && !*path) {
        return -LX_ENOENT;
    }
    /* -1 leaves one of the two as it is. */
    if ((int)uid != -1) {
        which |= QUARK_VFS_ATTR_UID;
    }
    if ((int)gid != -1) {
        which |= QUARK_VFS_ATTR_GID;
    }
    if (!which) {
        return 0;
    }
    return set_attrs(dirfd, path, nofollow, which, attrs);
}

#define LX_UTIME_NOW  0x3fffffffL
#define LX_UTIME_OMIT 0x3ffffffeL

/* utimensat: `times` is two timespecs, access then modification, or NULL for
   now and now. A file's times here are whole seconds. */
long __quark_utimens(long dirfd, const char *path, const long *times, int nofollow) {
    unsigned long attrs[5] = {0, 0, 0, 0, 0};
    unsigned long which = 0;
    if (!times) {
        which = QUARK_VFS_ATTR_ATIME_NOW | QUARK_VFS_ATTR_MTIME_NOW;
    } else {
        if (times[1] == LX_UTIME_NOW) {
            which |= QUARK_VFS_ATTR_ATIME_NOW;
        } else if (times[1] != LX_UTIME_OMIT) {
            which |= QUARK_VFS_ATTR_ATIME;
            attrs[3] = (unsigned long)times[0];
        }
        if (times[3] == LX_UTIME_NOW) {
            which |= QUARK_VFS_ATTR_MTIME_NOW;
        } else if (times[3] != LX_UTIME_OMIT) {
            which |= QUARK_VFS_ATTR_MTIME;
            attrs[4] = (unsigned long)times[2];
        }
    }
    if (!which) {
        return 0;
    }
    return set_attrs(dirfd, path, nofollow, which, attrs);
}

/* `fcntl`, for the commands a program on this system can actually be answered.
 *
 * It used to return 0 for everything, on the theory that musl only probes it.
 * That is true of musl and false of everything above it: libwayland duplicates
 * a descriptor with F_DUPFD_CLOEXEC before sending it, believed the 0, and sent
 * descriptor zero -- its own standard input -- to the compositor. A stub that
 * answers "fine" to a question it did not understand is worse than one that
 * answers "no", because the caller has no way to find out. */
#define LX_F_DUPFD          0
#define LX_F_GETFD          1
#define LX_F_SETFD          2
#define LX_F_GETFL          3
#define LX_F_SETFL          4
#define LX_F_DUPFD_CLOEXEC  1030
#define LX_F_GETLK          5
#define LX_F_SETLK          6
#define LX_F_SETLKW         7
#define LX_F_OFD_GETLK      36
#define LX_F_OFD_SETLK      37
#define LX_F_OFD_SETLKW     38
#define LX_F_RDLCK          0
#define LX_F_WRLCK          1
#define LX_F_UNLCK          2
#define LX_SEEK_SET         0
#define LX_SEEK_CUR         1
#define LX_SEEK_END         2

/* Linux's struct flock on x86-64. */
struct lx_flock {
    short l_type;
    short l_whence;
    long l_start;
    long l_len;
    int l_pid;
};

static long lock_error(int err) {
    switch (err) {
    case QUARK_VFS_WOULD_BLOCK: return -LX_EAGAIN;
    case QUARK_VFS_DEADLOCK:    return -LX_EDEADLK;
    case QUARK_VFS_NO_SPACE:    return -LX_ENOLCK;
    default:                    return vfs_errno(err);
    }
}

/* fcntl's record locks. The F_OFD_ forms belong to the open file, the others
   to the program; the server keeps both. */
static long file_lock(long fd, long cmd, struct lx_flock *fl) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_EINVAL);
    }
    if (!fl) {
        return -LX_EFAULT;
    }
    int ofd = cmd == LX_F_OFD_GETLK || cmd == LX_F_OFD_SETLK || cmd == LX_F_OFD_SETLKW;
    int query = cmd == LX_F_GETLK || cmd == LX_F_OFD_GETLK;
    int wait = cmd == LX_F_SETLKW || cmd == LX_F_OFD_SETLKW;
    if (ofd && fl->l_pid != 0) {
        return -LX_EINVAL;
    }
    unsigned long kind_of;
    switch (fl->l_type) {
    case LX_F_RDLCK: kind_of = 1; break;
    case LX_F_WRLCK: kind_of = 2; break;
    case LX_F_UNLCK: kind_of = 0; break;
    default: return -LX_EINVAL;
    }
    if (query && kind_of == 0) {
        return -LX_EINVAL;
    }
    long base;
    switch (fl->l_whence) {
    case LX_SEEK_SET:
        base = 0;
        break;
    case LX_SEEK_CUR:
    case LX_SEEK_END: {
        /* Where the descriptor is, or where the file ends: the server's to
           say, and a seek that goes nowhere says it. */
        unsigned long at;
        int err = quark_vfs_seek(h, 0, QUARK_VFS_SEEK_CUR, &at, 0);
        if (!err && fl->l_whence == LX_SEEK_END) {
            struct quark_vfs_stat r;
            err = quark_vfs_stat(h, &r);
            at = r.size;
        }
        if (err) {
            return vfs_errno(err);
        }
        base = (long)at;
        break;
    }
    default:
        return -LX_EINVAL;
    }
    long start = base + fl->l_start;
    long len = fl->l_len;
    /* A negative length covers the bytes before the start. */
    if (len < 0) {
        start += len;
        len = -len;
    }
    if (start < 0) {
        return -LX_EINVAL;
    }
    unsigned long flags = (ofd ? QUARK_VFS_LOCK_OFD : 0) | (wait ? QUARK_VFS_LOCK_WAIT : 0) |
                          (query ? QUARK_VFS_LOCK_QUERY : 0);
    unsigned long out[4];
    int err = quark_vfs_lock(h, kind_of, (unsigned long)start, (unsigned long)len, flags, out);
    if (err) {
        return lock_error(err);
    }
    if (!ofd && !query && kind_of != 0) {
        posix_locks_taken = 1;
    }
    if (query) {
        if (out[0] == 0) {
            fl->l_type = LX_F_UNLCK;
        } else {
            fl->l_type = out[0] == 2 ? LX_F_WRLCK : LX_F_RDLCK;
            fl->l_whence = LX_SEEK_SET;
            fl->l_start = (long)out[1];
            fl->l_len = (long)out[2];
            /* Linux names an open file's lock's holder -1; a program's is
               named by its program id. */
            fl->l_pid = out[3] == ~0UL ? -1 : (int)out[3];
        }
    }
    return 0;
}

#define LX_LOCK_SH 1
#define LX_LOCK_EX 2
#define LX_LOCK_NB 4
#define LX_LOCK_UN 8

/* A capability to map the file `fd` names, for mmap: `*cap` is the CSpace
   slot it was granted into. Writing through a shared mapping needs a
   descriptor open for writing, as on Linux, and the server is what checks. */
long __quark_file_map(long fd, int write_shared, unsigned long *cap) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_ENODEV);
    }
    unsigned long size;
    int err = quark_vfs_map(h, write_shared, cap, &size);
    if (err == QUARK_VFS_NOT_SUPPORTED) {
        return -LX_ENODEV;
    }
    if (err == QUARK_VFS_PERMISSION) {
        return -LX_EACCES;
    }
    return err ? vfs_errno(err) : 0;
}

/* flock: a lock on the whole file, belonging to the open file, as Linux has
   it — and so shared with a forked child, which holds the same open file.
   Unlike Linux's, it and fcntl's locks are one kind and can collide. */
long __quark_flock(long fd, long op) {
    unsigned long h;
    if (!is_file(fd, &h)) {
        return not_file(fd, -LX_EINVAL);
    }
    unsigned long kind_of;
    switch (op & ~LX_LOCK_NB) {
    case LX_LOCK_SH: kind_of = 1; break;
    case LX_LOCK_EX: kind_of = 2; break;
    case LX_LOCK_UN: kind_of = 0; break;
    default: return -LX_EINVAL;
    }
    unsigned long flags = QUARK_VFS_LOCK_OFD | ((op & LX_LOCK_NB) ? 0 : QUARK_VFS_LOCK_WAIT);
    int err = quark_vfs_lock(h, kind_of, 0, 0, flags, 0);
    return err ? lock_error(err) : 0;
}

/* Which descriptors a program has asked to be non-blocking. One bit per
   descriptor; a file's means nothing, because the VFS is a synchronous call
   and there is nothing to wait for. It is this program's own note and does
   not outlive it: what a program is exec'd into holding starts as waiting. */
static unsigned long nonblock_mask;

/* Say that a descriptor was made non-blocking when it was created, which is
   what `pipe2` and `eventfd` take a flag for. */
void __quark_fd_set_nonblock(long fd, int on) {
    if (fd < 0 || fd >= MAX_FDS) {
        return;
    }
    if (on) {
        nonblock_mask |= 1ul << fd;
    } else {
        nonblock_mask &= ~(1ul << fd);
    }
}

int __quark_fd_is_nonblock(long fd) {
    return fd >= 0 && fd < MAX_FDS && (nonblock_mask & (1ul << fd)) != 0;
}

#define LX_FD_CLOEXEC 1

long __quark_fcntl(long fd, long cmd, long arg) {
    if (fd < 0 || fd >= MAX_FDS) {
        return -LX_EBADF;
    }
    switch (cmd) {
    case LX_F_DUPFD:
    case LX_F_DUPFD_CLOEXEC: {
        if (arg < 0 || arg >= MAX_FDS) {
            return -LX_EINVAL;
        }
        unsigned long r = __syscall4(SYS_FD_DUP, __syscall0(SYS_GETPID),
                                     QUARK_ANY_FD, (unsigned long)fd, (unsigned long)arg);
        if (r == QUARK_ERR) {
            return is_open(fd) ? -LX_EMFILE : -LX_EBADF;
        }
        __quark_fd_forget((long)r);
        __quark_fd_set_nonblock((long)r, __quark_fd_is_nonblock(fd));
        if (cmd == LX_F_DUPFD_CLOEXEC) {
            __syscall3(SYS_FD_FLAGS, r, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
        }
        return (long)r;
    }
    case LX_F_GETLK:
    case LX_F_SETLK:
    case LX_F_SETLKW:
    case LX_F_OFD_GETLK:
    case LX_F_OFD_SETLK:
    case LX_F_OFD_SETLKW:
        return file_lock(fd, cmd, (struct lx_flock *)arg);
    /* FD_CLOEXEC, which is the descriptor's and the kernel's to keep: it is
       the kernel that closes it when this program becomes another. */
    case LX_F_GETFD: {
        unsigned long f = __syscall3(SYS_FD_FLAGS, (unsigned long)fd, QUARK_FD_GETFLAGS, 0);
        if (f == QUARK_ERR) {
            return -LX_EBADF;
        }
        return (f & QUARK_FD_CLOEXEC) ? LX_FD_CLOEXEC : 0;
    }
    case LX_F_SETFD:
        return __syscall3(SYS_FD_FLAGS, (unsigned long)fd, QUARK_FD_SETFLAGS,
                          (arg & LX_FD_CLOEXEC) ? QUARK_FD_CLOEXEC : 0) == QUARK_ERR
                   ? -LX_EBADF
                   : 0;
    case LX_F_GETFL: {
        unsigned long h;
        if (is_file(fd, &h)) {
            /* What it was opened to do is the server's to say. */
            unsigned long how = 0;
            if (quark_vfs_seek(h, 0, QUARK_VFS_SEEK_CUR, 0, &how)) {
                return -LX_EBADF;
            }
            long flags = (how & 3) == 3 ? LX_O_RDWR : (how & 2) ? LX_O_WRONLY : 0;
            return flags | ((how & 4) ? LX_O_APPEND : 0);
        }
        /* An end of a pipe reads or writes and never both, and a program
           that asks is usually asking which. */
        unsigned long k = __syscall1(SYS_FD_KIND, (unsigned long)fd);
        if (k == QUARK_ERR) {
            return -LX_EBADF;
        }
        long access = QUARK_FD_KIND(k) == QUARK_FD_KIND_PIPE_READ    ? 0
                      : QUARK_FD_KIND(k) == QUARK_FD_KIND_PIPE_WRITE ? LX_O_WRONLY
                                                                     : LX_O_RDWR;
        return access | (__quark_fd_is_nonblock(fd) ? LX_O_NONBLOCK : 0);
    }
    case LX_F_SETFL:
        if (!is_open(fd)) {
            return -LX_EBADF;
        }
        __quark_fd_set_nonblock(fd, (arg & LX_O_NONBLOCK) != 0);
        return 0;
    default:
        return -LX_EINVAL;
    }
}
