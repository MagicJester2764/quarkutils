/* The Linux system call surface, answered by Quark.
 *
 * musl is written against Linux. Not against "a kernel" — against Linux's
 * numbers, its argument order, its error convention and its idea of what a
 * process is. Porting it to a system whose calls are a different set with
 * different semantics is therefore not a port at all: it is a translation
 * layer, and this is it.
 *
 * The shape of the translation is the interesting part. Quark is a
 * microkernel, so most of what Linux calls a system call is not one here:
 * `open` and `read` are messages to the VFS, `write` on a descriptor is a
 * message to whatever is on the other end of it. The layer is mostly an IPC
 * client wearing Linux's numbers, which is the same thing the C library was
 * already — one level further down.
 *
 * Anything not translated returns -ENOSYS rather than pretending. A libc that
 * is told "no" can usually carry on; one that is told "yes" and handed a lie
 * fails somewhere unrelated and much later.
 */

#include <quark/layout.h>
#include <quark/syscall.h>

#include "abi.h"

typedef long ssize_t;
typedef unsigned long size_t;

#define NULL ((void *)0)

/* Linux x86-64 numbers, only the ones that are answered or deliberately
   refused. The rest fall through to -ENOSYS by not being here. */
#define LX_read              0
#define LX_select           23
#define LX_sched_yield      24
#define LX_fsync            74
#define LX_fdatasync        75
#define LX_chmod            90
#define LX_fchmod           91
#define LX_chown            92
#define LX_fchown           93
#define LX_lchown           94
#define LX_umask            95
#define LX_getrlimit        97
#define LX_getrusage        98
#define LX_sysinfo          99
#define LX_times           100
#define LX_setpgid         109
#define LX_getpgrp         111
#define LX_getgroups       115
#define LX_getpgid         121
#define LX_getsid          124
#define LX_sigaltstack     131
#define LX_mknod           133
#define LX_getpriority     140
#define LX_setpriority     141
#define LX_setrlimit       160
#define LX_sync            162
#define LX_setxattr        188
#define LX_lsetxattr       189
#define LX_fsetxattr       190
#define LX_getxattr        191
#define LX_lgetxattr       192
#define LX_fgetxattr       193
#define LX_listxattr       194
#define LX_llistxattr      195
#define LX_flistxattr      196
#define LX_removexattr     197
#define LX_lremovexattr    198
#define LX_fremovexattr    199
#define LX_mknodat         259
#define LX_fchownat        260
#define LX_fchmodat        268
#define LX_pselect6        270
#define LX_utimensat       280
#define LX_syncfs          306
#define LX_fchmodat2       452
#define LX_write             1
#define LX_open              2
#define LX_close             3
#define LX_poll              7
#define LX_stat              4
#define LX_access           21
#define LX_fstat             5
#define LX_lstat             6
#define LX_lseek             8
#define LX_mmap              9
#define LX_mprotect         10
#define LX_munmap           11
#define LX_msync            26
#define LX_brk              12
#define LX_rt_sigaction     13
#define LX_rt_sigprocmask   14
#define LX_rt_sigreturn     15
#define LX_pause            34
#define LX_getitimer        36
#define LX_alarm            37
#define LX_setitimer        38
#define LX_kill             62
#define LX_rt_sigpending   127
#define LX_rt_sigtimedwait 128
#define LX_rt_sigsuspend   130
#define LX_tkill           200
#define LX_tgkill          234
#define LX_epoll_pwait     281
#define LX_ioctl            16
#define LX_readv            19
#define LX_writev           20
#define LX_madvise          28
#define LX_sendmsg          46
#define LX_recvmsg          47
#define LX_pipe              22
#define LX_eventfd         284
#define LX_eventfd2        290
#define LX_pipe2           293
#define LX_socketpair       53
#define LX_socket           41
#define LX_fadvise64       221
#define LX_getpid           39
#define LX_getppid         110
#define LX_wait4            61
#define LX_fork             57
#define LX_execve           59
#define LX_vfork            58
#define LX_clone            56
#define LX_setsid          112
#define LX_timerfd_create  283
#define LX_timerfd_settime 286
#define LX_timerfd_gettime 287
#define LX_fcntl            72
#define LX_flock            73
#define LX_exit             60
#define LX_uname            63
#define LX_getcwd           79
#define LX_chdir            80
#define LX_fchdir           81
#define LX_getuid          102
#define LX_getgid          104
#define LX_setuid          105
#define LX_setgid          106
#define LX_geteuid         107
#define LX_getegid         108
#define LX_setreuid        113
#define LX_setregid        114
#define LX_setgroups       116
#define LX_setresuid       117
#define LX_getresuid       118
#define LX_setresgid       119
#define LX_getresgid       120
#define LX_setfsuid        122
#define LX_setfsgid        123
#define LX_arch_prctl      158
#define LX_sched_getaffinity 204
#define LX_getcpu          309
#define LX_futex           202
#define LX_gettid          186
#define LX_set_tid_address 218
#define LX_clock_gettime   228
#define LX_exit_group      231
#define LX_openat          257
#define LX_faccessat       269
#define LX_set_robust_list 273
#define LX_prlimit64       302
#define LX_getrandom       318
#define LX_newfstatat      262
#define LX_statx           332
#define LX_rseq            334
#define LX_epoll_wait      232
#define LX_epoll_ctl       233
#define LX_ppoll           271
#define LX_epoll_create1   291
#define LX_ftruncate       77
#define LX_fallocate      285
#define LX_memfd_create    319
#define LX_faccessat2      439
#define LX_mkdir            83
#define LX_mkdirat         258
#define LX_truncate         76
#define LX_rename           82
#define LX_rmdir            84
#define LX_link             86
#define LX_unlink           87
#define LX_unlinkat        263
#define LX_renameat        264
#define LX_linkat          265
#define LX_renameat2       316
#define LX_AT_REMOVEDIR  0x200
#define LX_getdents64      217
#define LX_dup              32
#define LX_pread64          17
#define LX_pwrite64         18
#define LX_dup2             33
#define LX_dup3            292
#define LX_O_CLOEXEC  02000000
#define LX_readlink         89
#define LX_symlink          88
#define LX_symlinkat       266
#define LX_readlinkat      267
#define LX_statfs          137
#define LX_fstatfs         138

#define LX_CLOCK_REALTIME        0
#define LX_CLOCK_REALTIME_COARSE 5
#define LX_CLOCK_TAI             11
#define LX_CLOCK_MONOTONIC       1
#define LX_CLOCK_PROCESS_CPUTIME 2
#define LX_CLOCK_THREAD_CPUTIME  3
#define LX_TIMER_ABSTIME         1
#define LX_nanosleep            35
#define LX_clock_nanosleep     230

#define ARCH_SET_FS 0x1002
#define ARCH_GET_FS 0x1003

/* Where anonymous mappings go.
 *
 * Linux's mmap picks an address; Quark's maps one you name, because a task
 * knows its own address space and the kernel has no business guessing. So the
 * choosing happens here, as a bump allocator over a region nothing else uses.
 * Nothing reclaims a hole left by munmap: a libc allocator returns memory to
 * itself far more often than it returns it here, and an address space this
 * size is not the scarce thing. */
#define MMAP_BASE   0x93000000000UL
#define MMAP_LIMIT  0x9F000000000UL
static unsigned long mmap_next = MMAP_BASE;

#define PAGE_SIZE 4096UL
/* What sys_munmap will take in one call. */
#define MAP_CHUNK 256UL

static void unmap_pages(unsigned long at, unsigned long pages) {
    unsigned long done = 0;
    while (done < pages) {
        unsigned long n = pages - done;
        if (n > MAP_CHUNK) {
            n = MAP_CHUNK;
        }
        __syscall2(SYS_MUNMAP, at + done * PAGE_SIZE, n);
        done += n;
    }
}


#ifdef QUARK_ABI_TRACE
/* A porting aid, off unless asked for: an unimplemented call otherwise reaches
   the program as a bare errno and is reported as whatever it was doing at the
   time ("sort: cannot read"), with nothing saying which call was missing. */
long __quark_write(long fd, const void *buf, unsigned long n);
static void trace(const char *what, long n) {
    char buf[64];
    int i = 0;
    buf[i++] = '[';
    while (*what && i < 40) {
        buf[i++] = *what++;
    }
    buf[i++] = ' ';
    if (n < 0) { buf[i++] = '-'; n = -n; }
    char d[24];
    int j = 0;
    do { d[j++] = (char)('0' + n % 10); n /= 10; } while (n && j < 20);
    while (j) { buf[i++] = d[--j]; }
    buf[i++] = ']';
    buf[i++] = '\n';
    __quark_write(2, buf, (unsigned long)i);
}
#else
#define trace(what, n) ((void)0)
#endif

#define LX_MAP_POPULATE  0x8000
#define LX_MAP_NORESERVE 0x4000

/* Anonymous memory is reserved whole and given its frames as it is touched,
   as Linux gives them: a mapping far bigger than what is used costs what is
   used. As Linux's overcommit heuristic does, a mapping bigger than the whole
   machine is refused unless MAP_NORESERVE says the program knows — calloc of
   a size nothing could hold has to come back NULL, not succeed and then die
   reading it. MAP_POPULATE asks for all of it now, which can be refused. */
static long do_mmap(unsigned long len, long flags) {
    if (len == 0) {
        return -LX_EINVAL;
    }
    unsigned long pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    unsigned long at = mmap_next;
    if (pages > (MMAP_LIMIT - at) / PAGE_SIZE) {
        trace("mmap-arena-full", (long)pages);
        return -LX_ENOMEM;
    }
    unsigned long how = (flags & LX_MAP_POPULATE) ? QUARK_MAP_POPULATE : 0;
    if (!(flags & LX_MAP_NORESERVE)) {
        how |= QUARK_MAP_ACCOUNT;
    }
    if (__syscall3(SYS_MAP_ANON, at, pages, how) == QUARK_ERR) {
        trace("mmap-failed-pages", (long)pages);
        return -LX_ENOMEM;
    }
    mmap_next = at + pages * PAGE_SIZE;
    return (long)at;
}

/* Map memory named by a descriptor, at an address of our choosing. */
static long do_mmap_fd(long fd, unsigned long len) {
    if (len == 0) {
        return -LX_EINVAL;
    }
    unsigned long pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    unsigned long at = mmap_next;
    if (at + pages * PAGE_SIZE > MMAP_LIMIT) {
        return -LX_ENOMEM;
    }
    /* The whole region is mapped, whatever the caller asked to see of it --
       Quark has no partial mapping of a descriptor. The call says how much
       that was, and the arena must step over all of it: advancing by the
       caller's length instead would hand the next mapping addresses that are
       already occupied, and the kernel refuses to map over them. */
    unsigned long got = __syscall2(SYS_MMAP_FD, (unsigned long)fd, at);
    if (got == QUARK_ERR) {
        return -LX_ENODEV;
    }
    unsigned long real = (got + PAGE_SIZE - 1) / PAGE_SIZE;
    mmap_next = at + (real > pages ? real : pages) * PAGE_SIZE;
    return (long)at;
}

#define LX_PROT_WRITE   2
#define LX_PROT_EXEC    4
#define LX_MAP_SHARED   1

/* A file, through the VFS: a memory object whose pages the server provides
   as they are touched. The capability it grants is only needed to make the
   mapping, which keeps the object; it goes straight after. */
static long do_mmap_file(long fd, unsigned long len, long prot, long flags, long off) {
    if (len == 0 || off < 0 || (off & (PAGE_SIZE - 1))) {
        return -LX_EINVAL;
    }
    unsigned long pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    unsigned long at = mmap_next;
    if (pages > (MMAP_LIMIT - at) / PAGE_SIZE) {
        return -LX_ENOMEM;
    }
    int shared = (flags & LX_MAP_SHARED) != 0;
    int write = (prot & LX_PROT_WRITE) != 0;
    unsigned long slot;
    long bad = __quark_file_map(fd, shared && write, &slot);
    if (bad) {
        return bad;
    }
    unsigned long how = (write ? QUARK_OBJECT_WRITE : 0) | (shared ? QUARK_OBJECT_SHARED : 0) |
                        ((prot & LX_PROT_EXEC) ? QUARK_OBJECT_EXEC : 0);
    unsigned long r = __syscall5(SYS_OBJECT_MAP, slot, at, pages,
                                 (unsigned long)off / PAGE_SIZE, how);
    __syscall1(SYS_CAP_DELETE, slot);
    if (r == QUARK_ERR) {
        return -LX_ENOMEM;
    }
    mmap_next = at + pages * PAGE_SIZE;
    return (long)at;
}

static long do_munmap(unsigned long at, unsigned long len) {
    unmap_pages(at, (len + PAGE_SIZE - 1) / PAGE_SIZE);
    return 0;
}

struct iovec {
    void  *iov_base;
    size_t iov_len;
};

/* Descriptors 0, 1 and 2 go to the kernel and anything above them to the VFS;
   `files.c` decides which, so that `write` and `writev` agree about it. */
#define do_write __quark_write
#define do_read  __quark_read

/* Linux's struct timespec, which is what clock_gettime fills in. */
struct lx_timespec {
    long tv_sec;
    long tv_nsec;
};

/* What the kernel said about a process group or a session, as Linux says
   it: the number, or why not. */
static long job_answer(unsigned long r) {
    if (r == QUARK_ERR) {
        return -LX_ESRCH;
    }
    if (r == QUARK_NOT_ALLOWED) {
        return -LX_EPERM;
    }
    return (long)r;
}

/* Sleep until `req` has passed on `clock` — or, with TIMER_ABSTIME, until
   the clock reads `req`. Time here is a 100 Hz tick, so a sleep is rounded
   up to whole ticks and one more, never ending early. It waits by receiving
   from itself, which nobody sends to. */
static long do_sleep(long clock, long flags, const struct lx_timespec *req,
                     struct lx_timespec *rem) {
    if (!req) {
        return -LX_EFAULT;
    }
    if (req->tv_sec < 0 || req->tv_nsec < 0 || req->tv_nsec >= 1000000000L) {
        return -LX_EINVAL;
    }
    if (clock == LX_CLOCK_PROCESS_CPUTIME || clock == LX_CLOCK_THREAD_CPUTIME || clock < 0) {
        return -LX_EINVAL;
    }
    unsigned long now = __syscall0(SYS_TICKS);
    unsigned long ticks = (unsigned long)req->tv_sec * 100 +
                          ((unsigned long)req->tv_nsec + 9999999UL) / 10000000UL;
    unsigned long deadline;
    if (flags & LX_TIMER_ABSTIME) {
        long boot = 0;
        if (clock == LX_CLOCK_REALTIME || clock == LX_CLOCK_REALTIME_COARSE || clock == LX_CLOCK_TAI) {
            boot = (long)__syscall0(SYS_BOOT_TIME);
        }
        long since_boot = req->tv_sec - boot;
        if (since_boot < 0) {
            return 0;
        }
        deadline = (unsigned long)since_boot * 100 +
                   ((unsigned long)req->tv_nsec + 9999999UL) / 10000000UL;
    } else {
        deadline = now + ticks + (ticks ? 1 : 0);
    }
    unsigned long self = __syscall0(SYS_GETPID);
    while ((now = __syscall0(SYS_TICKS)) < deadline) {
        struct quark_msg m;
        unsigned long r = __syscall3(SYS_RECV_TIMEOUT, self, (unsigned long)&m, deadline - now);
        /* A signal ended the sleep. If a handler ran, that is the sleep over,
           with what was left of it said — a sleep is never taken up again,
           whatever the handler asked for. If none did, the signal is blocked
           or was not this thread's to take, and there is the rest of the
           sleep still to do. */
        if (r == QUARK_SLEEP_INTERRUPTED && (__quark_sig_interrupted() & QUARK_SIG_RAN)) {
            if (rem && !(flags & LX_TIMER_ABSTIME)) {
                now = __syscall0(SYS_TICKS);
                unsigned long left = now < deadline ? deadline - now : 0;
                rem->tv_sec = (long)(left / 100);
                rem->tv_nsec = (long)(left % 100) * 10000000L;
            }
            return -LX_EINTR;
        }
    }
    return 0;
}

/* Setting the thread pointer arrives here rather than through the numbered
 * table: musl issues it from assembly, because on Linux it is one instruction
 * and not worth a call. Quark has a system call for exactly this, so it is a
 * direct translation and the shortest path in the whole layer. */
long __quark_set_fs(unsigned long tp);

long __quark_set_fs(unsigned long tp) {
    if (__syscall1(SYS_SET_FS_BASE, tp) == QUARK_ERR) {
        return -LX_EINVAL;
    }
    return 0;
}

/* uname: six fields of 65 bytes. The release is the kernel's ABI version,
   which is the thing a program built for Quark would want to compare. */
static void put_field(char *field, const char *text) {
    int i = 0;
    for (; text[i] && i < 64; i++) {
        field[i] = text[i];
    }
    for (; i < 65; i++) {
        field[i] = 0;
    }
}

static long do_uname(char *u) {
    if (!u) {
        return -LX_EFAULT;
    }
    unsigned long v = __syscall0(SYS_ABI_VERSION);
    char release[16];
    int n = 0;
    unsigned long parts[2] = { v >> 16, v & 0xffff };
    for (int p = 0; p < 2; p++) {
        char digits[8];
        int d = 0;
        unsigned long x = parts[p];
        do {
            digits[d++] = (char)('0' + x % 10);
            x /= 10;
        } while (x && d < 7);
        while (d) {
            release[n++] = digits[--d];
        }
        if (p == 0) {
            release[n++] = '.';
        }
    }
    release[n] = 0;
    put_field(u, "Quark");
    put_field(u + 65, "quark");
    put_field(u + 130, release);
    put_field(u + 195, "Quark microkernel");
    put_field(u + 260, "x86_64");
    put_field(u + 325, "(none)");
    return 0;
}

#define LX_RLIMIT_STACK  3
#define LX_RLIMIT_NOFILE 7
#define LX_RLIM_INFINITY (~0UL)

static long do_getrlimit(long what, unsigned long *lim) {
    if (!lim) {
        return -LX_EFAULT;
    }
    unsigned long v;
    switch (what) {
    case LX_RLIMIT_NOFILE: v = 64; break;           /* the kernel's table */
    case LX_RLIMIT_STACK:  v = 256 * 4096UL; break; /* what a spawner gives */
    default:               v = LX_RLIM_INFINITY; break;
    }
    lim[0] = v; /* soft */
    lim[1] = v; /* hard */
    return 0;
}

/* Linux's struct sysinfo: uptime, three load averages, then memory in units
   of `mem_unit` bytes. The kernel says how many frames are free; how many
   there are in all it does not, so the total is what QEMU is given. */
static long do_sysinfo(unsigned long *out) {
    if (!out) {
        return -LX_EFAULT;
    }
    for (int i = 0; i < 14; i++) {
        out[i] = 0;
    }
    unsigned long mem = __syscall0(SYS_MEM_INFO);
    unsigned long free_frames = mem == QUARK_ERR ? 0 : mem >> 32;
    out[0] = __syscall0(SYS_TICKS) / 100;      /* uptime */
    out[4] = free_frames + (mem & 0xFFFFFFFF); /* totalram: at least what is in use here */
    out[5] = free_frames;                      /* freeram */
    /* procs (a short) and padding share the ninth word; mem_unit is an int
       after totalhigh and freehigh. */
    out[9] = 1;
    ((unsigned int *)out)[26] = 4096;          /* mem_unit */
    return 0;
}

/* select, over the same wait poll uses. An fd_set is an array of words, one
   bit a descriptor. */
static long do_select(long nfds, unsigned long *rd, unsigned long *wr, unsigned long *ex,
                      long timeout_ms) {
    struct { int fd; short events; short revents; } p[32];
    long n = 0;
    if (nfds < 0 || nfds > 1024) {
        return -LX_EINVAL;
    }
    for (long fd = 0; fd < nfds; fd++) {
        unsigned long bit = 1UL << (fd % 64);
        long word = fd / 64;
        short ev = 0;
        if (rd && (rd[word] & bit)) {
            ev |= 0x001; /* POLLIN */
        }
        if (wr && (wr[word] & bit)) {
            ev |= 0x004; /* POLLOUT */
        }
        if (ex && (ex[word] & bit)) {
            ev |= 0x002; /* POLLPRI: nothing here is ever exceptional */
        }
        if (!ev) {
            continue;
        }
        if (n == 32) {
            return -LX_EINVAL;
        }
        p[n].fd = (int)fd;
        p[n].events = ev;
        p[n].revents = 0;
        n++;
    }
    long got = __quark_poll(p, n, timeout_ms);
    if (got < 0) {
        return got;
    }
    for (long word = 0; word * 64 < nfds; word++) {
        if (rd) {
            rd[word] = 0;
        }
        if (wr) {
            wr[word] = 0;
        }
        if (ex) {
            ex[word] = 0;
        }
    }
    long count = 0;
    for (long i = 0; i < n; i++) {
        unsigned long bit = 1UL << (p[i].fd % 64);
        long word = p[i].fd / 64;
        if (p[i].revents & 0x020 /* POLLNVAL */) {
            return -LX_EBADF;
        }
        /* A hangup is readable: the read that follows says end of file. */
        if (rd && (p[i].events & 0x001) && (p[i].revents & (0x001 | 0x010 | 0x008))) {
            rd[word] |= bit;
            count++;
        }
        if (wr && (p[i].events & 0x004) && (p[i].revents & (0x004 | 0x008))) {
            wr[word] |= bit;
            count++;
        }
    }
    return count;
}

/* A length of time as ticks, for an alarm: rounded up to a whole tick and
   then one more, because the tick under way is part of the way through and a
   timer must not run out early. No time at all is no ticks, which is how an
   alarm is turned off. */
static unsigned long time_ticks(unsigned long sec, unsigned long usec) {
    unsigned long ticks = sec * 100 + (usec + 9999) / 10000;
    return ticks ? ticks + 1 : 0;
}

/* And the ticks left of an alarm as the time left of it: without that one
   more, or `alarm(10)` asked at once how long it has would say eleven. One
   tick left is still one: none means there is no alarm. */
static unsigned long ticks_left(unsigned long ticks) {
    return ticks > 1 ? ticks - 1 : ticks;
}

/* setitimer and getitimer. An itimerval is two timevals, the interval and
   then what is left, each seconds and microseconds.

   ITIMER_REAL is the kernel's alarm. The other two count the time a program
   spends running, which nothing here measures. */
static long do_itimer(long which, const long *set, long *old) {
    if (which != 0 /* ITIMER_REAL */) {
        return -LX_EINVAL;
    }
    unsigned long was;
    if (set) {
        if (set[0] < 0 || set[2] < 0 || set[1] < 0 || set[1] >= 1000000 || set[3] < 0
            || set[3] >= 1000000) {
            return -LX_EINVAL;
        }
        /* A repeat is a period and not a wait: no tick is added to it, or a
           timer asked to go every fifty milliseconds would go every sixty. */
        unsigned long every = (unsigned long)set[0] * 100 + ((unsigned long)set[1] + 9999) / 10000;
        was = __syscall3(SYS_SIG_ALARM,
                         time_ticks((unsigned long)set[2], (unsigned long)set[3]), every, 0);
    } else {
        was = __syscall3(SYS_SIG_ALARM, 0, 0, QUARK_ALARM_ASK);
    }
    if (was == QUARK_ERR) {
        return -LX_EINVAL;
    }
    if (old) {
        unsigned long left = ticks_left(was & 0xFFFFFFFFUL), every = was >> 32;
        old[0] = (long)(every / 100);
        old[1] = (long)(every % 100) * 10000;
        old[2] = (long)(left / 100);
        old[3] = (long)(left % 100) * 10000;
    }
    return 0;
}

/* setuid and its relations: become `id`, by the kernel's call `how`, whose
   answer for this task is `shift` bits up in what SYS_GET_UID says. */
static long set_identity(unsigned long how, int shift, long id) {
    if ((int)id == -1) {
        return 0;
    }
    unsigned int now = (unsigned int)(__syscall0(SYS_GET_UID) >> shift);
    if ((unsigned int)id == now) {
        return 0;
    }
    return __syscall2(how, __syscall0(SYS_GETPID), (unsigned long)(unsigned int)id) == QUARK_ERR
               ? -LX_EPERM
               : 0;
}

/* Give up the right to say who a task is: every capability of that kind
 * this task holds.
 *
 * The kernel keeps one user for a task, and what lets a program change it is
 * a capability, not being user 0. So a program that was root and has made
 * itself somebody else still holds what would make it root again — and the
 * one thing `setuid` promises is that it cannot. Unix keeps a saved id to
 * decide that; here the capability is the saved id. It is kept across a
 * change that Unix would let a program undo (`seteuid`, which leaves the
 * real and saved ids alone) and given up at one it would not. */
static void forget_set_uid(void) {
    unsigned long me = __syscall0(SYS_GETPID);
    for (unsigned long slot = 0; slot < QUARK_CSPACE_SLOTS; slot++) {
        unsigned long cap[4];
        if (__syscall3(SYS_CAP_READ, me, slot, (unsigned long)cap) != QUARK_ERR && cap[3] &&
            cap[0] == QUARK_CAP_TYPE_SET_UID) {
            __syscall1(SYS_CAP_DELETE, slot);
        }
    }
}

/* Become user `id`; and if `for_good`, and that is not user 0, be unable to
   become anybody else afterwards. */
static long set_user(long id, int for_good) {
    long r = set_identity(SYS_SET_UID, 32, id);
    if (r == 0 && for_good && (unsigned int)(__syscall0(SYS_GET_UID) >> 32) != 0) {
        forget_set_uid();
    }
    return r;
}

/* Wait under another signal mask, as ppoll and pselect do: the mask goes in,
   whatever it lets through that was already waiting runs — and is an
   interruption, with no wait at all — and the mask comes back out. */
#define WAIT_UNDER(maskp, wait)                                          \
    do {                                                                 \
        const unsigned long *under_ = (maskp);                           \
        if (!under_) {                                                   \
            return (wait);                                               \
        }                                                                \
        unsigned long saved_ = __quark_sig_swap_mask(*under_);           \
        long r_ = (__quark_sig_deliver() & QUARK_SIG_RAN) ? -LX_EINTR : (wait); \
        __quark_sig_swap_mask(saved_);                                   \
        return r_;                                                       \
    } while (0)

static long dispatch(long n, long a1, long a2, long a3, long a4, long a5, long a6);

long __quark_syscall(long n, long a1, long a2, long a3, long a4, long a5, long a6);

/* Every call musl makes arrives here. A handler runs on the way out of one:
   the kernel has said a signal is waiting, or the call just made let one
   through — `sigprocmask`, `kill` at itself. */
long __quark_syscall(long n, long a1, long a2, long a3, long a4, long a5, long a6) {
    long r = dispatch(n, a1, a2, a3, a4, a5, a6);
    if (__quark_sig_due()) {
        __quark_sig_deliver();
    }
    return r;
}

static long dispatch(long n, long a1, long a2, long a3, long a4, long a5, long a6) {
    (void)a4; (void)a5; (void)a6;

    switch (n) {
    case LX_write:
        return do_write(a1, (const void *)a2, (unsigned long)a3);

    case LX_read:
        return do_read(a1, (void *)a2, (unsigned long)a3);

    case LX_writev: {
        const struct iovec *v = (const struct iovec *)a2;
        long total = 0;
        for (long i = 0; i < a3; i++) {
            if (v[i].iov_len == 0) {
                continue;
            }
            long w = do_write(a1, v[i].iov_base, v[i].iov_len);
            if (w < 0) {
                return total ? total : w;
            }
            total += w;
            if ((unsigned long)w < v[i].iov_len) {
                break; /* short write; the caller asks again */
            }
        }
        return total;
    }

    case LX_readv: {
        const struct iovec *v = (const struct iovec *)a2;
        long total = 0;
        for (long i = 0; i < a3; i++) {
            if (v[i].iov_len == 0) {
                continue;
            }
            long r = do_read(a1, v[i].iov_base, v[i].iov_len);
            if (r < 0) {
                return total ? total : r;
            }
            total += r;
            if ((unsigned long)r < v[i].iov_len) {
                break;
            }
        }
        return total;
    }

    case LX_mmap:
        /* A mapping backed by a descriptor is `wl_shm`: the caller made memory
           with memfd_create and wants it visible. Quark maps at an address you
           name, so the address is chosen here from the same arena anonymous
           mappings come out of.
           A mapping of a file is a memory object the VFS pages, below. */
        if (a5 >= 0 && __quark_fd_is_file(a5)) {
            return do_mmap_file(a5, (unsigned long)a2, a3, a4, a6);
        }
        if (a5 != -1L) {
            return do_mmap_fd(a5, (unsigned long)a2);
        }
        return do_mmap((unsigned long)a2, a4);

    case LX_munmap:
        return do_munmap((unsigned long)a1, (unsigned long)a2);

    /* What a shared mapping wrote reaches the file before this returns,
       whether it was asked for now (MS_SYNC) or eventually (MS_ASYNC):
       nothing here writes asynchronously. MS_INVALIDATE asks for nothing a
       mapping of the cache itself needs. */
    case LX_msync: {
        if ((a1 & (PAGE_SIZE - 1)) || (a3 & ~7L) || ((a3 & 1) && (a3 & 4))) {
            return -LX_EINVAL;
        }
        unsigned long pages = ((unsigned long)a2 + PAGE_SIZE - 1) / PAGE_SIZE;
        if (pages == 0) {
            return 0;
        }
        return __syscall2(SYS_OBJECT_SYNC, (unsigned long)a1, pages) == QUARK_ERR ? -LX_EIO : 0;
    }

    /* Nothing here has page permissions to change after the fact, and the
       mapping already allows what was asked for. Advice about how a file will
       be read is the same kind of thing: nothing here acts on it, and having
       done nothing is a complete implementation of it rather than a refusal. */
    case LX_mprotect:
    case LX_madvise:
    case LX_fadvise64:
        return 0;

    /* There is no brk. Saying so is what makes musl use mmap instead, which
       is the path that works. */
    case LX_brk:
        return -LX_ENOMEM;

    case LX_arch_prctl:
        if (a1 == ARCH_SET_FS) {
            __syscall1(SYS_SET_FS_BASE, (unsigned long)a2);
            return 0;
        }
        return -LX_EINVAL;

    /* Every task may run on every processor the kernel is using: nothing
       pins one yet, so the mask a task is asked for is all of them. That is
       what makes `nproc` right, and `sysconf(_SC_NPROCESSORS_ONLN)`, which
       is this call and a count of its bits. It said one for as long as there
       was one. */
    case LX_sched_getaffinity: {
        unsigned long size = (unsigned long)a2;
        unsigned char *mask = (unsigned char *)a3;
        if (!mask || size < sizeof(unsigned long)) {
            return -LX_EINVAL;
        }
        for (unsigned long i = 0; i < size; i++) {
            mask[i] = 0;
        }
        unsigned long cpus = __syscall0(SYS_CPUS) & 0xFFFFFFFFUL;
        for (unsigned long i = 0; i < cpus && i / 8 < size; i++) {
            mask[i / 8] |= (unsigned char)(1u << (i % 8));
        }
        return (long)sizeof(unsigned long);
    }

    /* Which processor this is, as of the question: a task is moved between
       them at any moment. There is one memory node. */
    case LX_getcpu: {
        unsigned *cpu = (unsigned *)a1;
        unsigned *node = (unsigned *)a2;
        if (cpu) {
            *cpu = (unsigned)(__syscall0(SYS_CPUS) >> 32);
        }
        if (node) {
            *node = 0;
        }
        return 0;
    }

    case LX_set_tid_address:
        // Not a formality: this is where the word to clear on exit is
        // registered, and musl's thread-list lock is that word.
        return (long)__syscall1(SYS_SET_CLEAR_TID, (unsigned long)a1);

    case LX_getpid:
        return __quark_getpid();
    case LX_gettid:
        /* A thread is its task. */
        return (long)__syscall0(SYS_GETPID);

    /* Waiting for a child.
     *
     * `SYS_WAIT` answers with the child's id and its exit code packed in one
     * word, and reaps it: by the time it returns, that child's memory is free
     * and its id may already belong to something else. Linux's status word is
     * a different shape — the exit code lives in bits 8 to 15, and the low
     * bits say whether it was a signal — so the translation is the whole of
     * what this does.
     *
     * A negative code is the kernel saying "killed", with the signal number
     * negated, which is Linux's low byte with no `WIFEXITED` bit above it. */
    case LX_wait4: {
        /* A child in particular; any (-1); or one of a process group — the
           caller's own (0) or the one named (less than -1). WNOHANG, which a
           shell tidying up after itself depends on not waiting; and
           WUNTRACED and WCONTINUED, which a shell with jobs uses to hear
           that one has stopped or been started again. */
        unsigned long how = QUARK_WAIT_BY_PID;
        unsigned long who = 0;
        if (a1 > 0) {
            who = (unsigned long)a1;
        } else if (a1 != -1) {
            how |= QUARK_WAIT_GROUP;
            who = (unsigned long)-a1;
        }
        if (a3 & 1 /* WNOHANG */) {
            how |= QUARK_WAIT_NOW;
        }
        if (a3 & 2 /* WUNTRACED */) {
            how |= QUARK_WAIT_STOPPED;
        }
        if (a3 & 8 /* WCONTINUED */) {
            how |= QUARK_WAIT_CONTINUED;
        }
        unsigned long got = __syscall2(SYS_WAIT_FOR, who, how);
        if (got == QUARK_ERR) {
            return -LX_ECHILD;
        }
        if (got == 0) {
            return 0; /* asked not to wait, and there is nothing to say */
        }
        long pid = (long)(got & 0x7FFFFFFFUL);
        int code = (int)(got >> 32);
        if (a2) {
            int *status = (int *)a2;
            if (got & QUARK_WAIT_REPORT) {
                /* Stopped by a signal, which Linux says as the signal above
                   0x7f; or started again, which it says as 0xffff. Still
                   there either way: nothing was collected. */
                *status = code ? ((code & 0xFF) << 8) | 0x7F : 0xFFFF;
            } else {
                *status = code < 0 ? (-code & 0x7F) : ((code & 0xFF) << 8);
            }
        }
        /* How long it ran is not kept. */
        if (a4) {
            unsigned char *usage = (unsigned char *)a4;
            for (int i = 0; i < 144; i++) {
                usage[i] = 0;
            }
        }
        return pid;
    }

    case LX_getppid: {
        /* The task that made this one, which the kernel knows: its answer
           about a task carries the parent above the state. */
        unsigned long info = __syscall1(SYS_TASK_INFO, __syscall0(SYS_GETPID));
        if (info == QUARK_ERR) {
            return 1;
        }
        unsigned long parent = __syscall1(SYS_PID, (info >> 4) & 0x0FFFFFFF);
        return parent == QUARK_ERR ? 1 : (long)parent;
    }

    /* Process groups and sessions are the kernel's, and are asked of it. */
    case LX_getpgrp:
        return job_answer(__syscall2(SYS_PGROUP, QUARK_PGROUP_GET, 0));
    case LX_getpgid:
        return a1 < 0 ? -LX_ESRCH
                      : job_answer(__syscall2(SYS_PGROUP, QUARK_PGROUP_GET, (unsigned long)a1));
    case LX_getsid:
        return a1 < 0 ? -LX_ESRCH
                      : job_answer(__syscall2(SYS_PGROUP, QUARK_SESSION_GET, (unsigned long)a1));
    case LX_setpgid:
        if (a1 < 0 || a2 < 0) {
            return -LX_EINVAL;
        }
        return job_answer(__syscall3(SYS_PGROUP, QUARK_PGROUP_SET, (unsigned long)a1,
                                     (unsigned long)a2));
    /* The groups a task is in besides its own. Asked with no room, it is
       being asked how many there are; with too little, that is an error and
       nothing is written. */
    case LX_getgroups: {
        unsigned int in[QUARK_MAX_GROUPS];
        unsigned long count =
            __syscall4(SYS_GROUPS, QUARK_GROUPS_GET, 0, (unsigned long)in, QUARK_MAX_GROUPS);
        if (count == QUARK_ERR || count > QUARK_MAX_GROUPS) {
            return -LX_EINVAL;
        }
        if (a1 == 0) {
            return (long)count;
        }
        if (a1 < 0 || (unsigned long)a1 < count) {
            return -LX_EINVAL;
        }
        if (!a2) {
            return -LX_EFAULT;
        }
        for (unsigned long i = 0; i < count; i++) {
            ((unsigned int *)a2)[i] = in[i];
        }
        return (long)count;
    }
    case LX_getpriority:
        return 20; /* the raw call's "nice 0" */
    case LX_setpriority:
        return 0;
    case LX_sched_yield:
        __syscall0(SYS_YIELD);
        return 0;
    case LX_sigaltstack:
        return 0;

    /* A process of one's own.
     *
     * musl calls `SYS_fork` on x86_64 and reaches `__clone` only for threads,
     * so this is the path an ordinary `fork()` takes. `vfork` is the same
     * thing here: its promise is that the parent is suspended until the child
     * execs or exits, and a real fork keeps every program that relies on that
     * working — more slowly, and without the sharp edges.
     *
     * `clone` arrives here only from a program calling it directly, since
     * musl's own thread path is a tail call into `__quark_clone`. Anything
     * asking for a new process with a shared file table or a stopped child is
     * asking for a Linux this is not. */
    /* Become another program. It does not return unless it failed, and what
       it answers then is what `execl` reports. */
    case LX_execve:
        return __quark_execve((const char *)a1, (char *const *)a2, (char *const *)a3);

    case LX_fork:
    case LX_vfork:
        return __quark_fork();
    case LX_clone: {
        unsigned long flags = (unsigned long)a1;
        if (flags & 0x00000100UL /* CLONE_VM */) {
            return -LX_ENOSYS;
        }
        return __quark_fork();
    }

    /* Who this is. The kernel keeps one user and one group for a task, and
       there is no set-user-id here to make the effective one differ: the
       real, effective and saved ids are all that one. It answered 0 to all
       four whoever asked, and never answered the three-at-once form at all —
       which is the one bash asks, so its prompt was for nobody. */
    case LX_getuid:
    case LX_geteuid:
        return (long)(__syscall0(SYS_GET_UID) >> 32);
    case LX_getgid:
    case LX_getegid:
        return (long)(__syscall0(SYS_GET_UID) & 0xFFFFFFFFUL);
    case LX_getresuid:
    case LX_getresgid: {
        unsigned long who = __syscall0(SYS_GET_UID);
        unsigned int id = n == LX_getresuid ? (unsigned int)(who >> 32) : (unsigned int)who;
        unsigned int *out[3] = { (unsigned int *)a1, (unsigned int *)a2, (unsigned int *)a3 };
        for (int i = 0; i < 3; i++) {
            if (!out[i]) {
                return -LX_EFAULT;
            }
            *out[i] = id;
        }
        return 0;
    }
    /* Becoming somebody else is a capability's to allow, and the kernel
       says no without it. Asking to be who one already is is always fine,
       which is all a program dropping privileges it has not got is doing.
       -1 in the forms that take several means "leave this one". */
    case LX_setuid:
        /* All three of Unix's ids at once: there is no way back. */
        return set_user(a1, 1);
    case LX_setfsuid:
        return set_user(a1, 0);
    case LX_setgid:
    case LX_setfsgid:
        return set_identity(SYS_SET_GID, 0, a1);
    /* The forms that name the ids one by one. The effective one is who the
       program is; it is for good when the real one goes with it — and, in
       the form that names the saved one, when that does too. A program that
       changes only the effective one is keeping the way back, and has it. */
    case LX_setreuid:
        return set_user((int)a2 != -1 ? a2 : a1, (int)a1 != -1 && (int)a1 != 0);
    case LX_setresuid:
        return set_user((int)a2 != -1 ? a2 : a1,
                        (int)a1 != -1 && (int)a1 != 0 && (int)a3 != -1 && (int)a3 != 0);
    case LX_setregid:
    case LX_setresgid:
        return set_identity(SYS_SET_GID, 0, (int)a2 != -1 ? a2 : a1);
    /* The groups besides its own, which are part of who a task is: the
       kernel refuses without the capability, as it does a change of user.
       Saying what is already so needs nothing. */
    case LX_setgroups: {
        if (a1 < 0 || a1 > QUARK_MAX_GROUPS) {
            return -LX_EINVAL;
        }
        if (a1 > 0 && !a2) {
            return -LX_EFAULT;
        }
        unsigned int in[QUARK_MAX_GROUPS];
        unsigned long count =
            __syscall4(SYS_GROUPS, QUARK_GROUPS_GET, 0, (unsigned long)in, QUARK_MAX_GROUPS);
        int same = count == (unsigned long)a1;
        for (long i = 0; same && i < a1; i++) {
            same = in[i] == ((const unsigned int *)a2)[i];
        }
        if (same) {
            return 0;
        }
        return __syscall4(SYS_GROUPS, QUARK_GROUPS_SET, 0, (unsigned long)a2, (unsigned long)a1) ==
                       QUARK_ERR
                   ? -LX_EPERM
                   : 0;
    }

    case LX_exit:
        /* One thread: `pthread_exit`, and what a thread's start routine
           returning comes to. */
        __syscall1(SYS_EXIT_CODE, (unsigned long)a1);
        for (;;) { }
    case LX_exit_group:
        /* The program. Ending only the caller left every other thread
           parked on a lock nobody would release, holding what the program
           had open. */
        __syscall1(SYS_EXIT_PROGRAM, (unsigned long)a1);
        for (;;) { }

    case LX_futex: {
        /* FUTEX_WAIT is 0 and FUTEX_WAKE is 1, with the private flag masked
           off: every process here has its own address space, so every futex
           is private already.
         *
         * The fourth argument is a *relative* timeout for FUTEX_WAIT, and the
         * answer matters as much as the wait: a caller that gave a deadline
         * asks which happened, so it is ETIMEDOUT when the time ran out and
         * EAGAIN when the word had already changed. Dropping the timeout made
         * every wait with a deadline wait for ever -- which is what
         * `g_cond_wait_until` and musl's `sem_timedwait` are built out of,
         * and glib's thread pool is built out of that. */
        long op = a2 & 0x7f;
        if (op == 0) {
            unsigned long r;
            if (a4) {
                const struct lx_timespec *ts = (const struct lx_timespec *)a4;
                /* Rounded up: the PIT ticks at 100 Hz, and a wait that came
                   back early would be a wait that did not happen. */
                unsigned long ticks =
                    (unsigned long)ts->tv_sec * 100 + (unsigned long)((ts->tv_nsec + 9999999) / 10000000);
                r = __syscall3(SYS_FUTEX_WAIT_TIMEOUT, (unsigned long)a1, (unsigned long)a3, ticks);
            } else {
                r = __syscall2(SYS_FUTEX_WAIT, (unsigned long)a1, (unsigned long)a3);
            }
            if (r == QUARK_ERR) {
                return -LX_EINVAL;
            }
            if (r == 1) {
                return -LX_EAGAIN;
            }
            if (r == 2) {
                return -LX_ETIMEDOUT;
            }
            return 0;
        }
        if (op == 1) {
            unsigned long woken = __syscall2(SYS_FUTEX_WAKE, (unsigned long)a1, (unsigned long)a3);
            return woken == QUARK_ERR ? -LX_EINVAL : (long)woken;
        }
        return -LX_ENOSYS;
    }

    case LX_clock_gettime: {
        struct lx_timespec *ts = (struct lx_timespec *)a2;
        if (!ts) {
            return -LX_EFAULT;
        }
        /* A 100 Hz tick counter, and the date the kernel read at boot. The
           real-time clocks add that date; every other clock counts from boot,
           which is what a monotonic clock is for. */
        unsigned long ticks = __syscall0(SYS_TICKS);
        long base = 0;
        if (a1 == LX_CLOCK_REALTIME || a1 == LX_CLOCK_REALTIME_COARSE || a1 == LX_CLOCK_TAI) {
            base = (long)__syscall0(SYS_BOOT_TIME);
        }
        ts->tv_sec = base + (long)(ticks / 100);
        ts->tv_nsec = (long)((ticks % 100) * 10000000L);
        return 0;
    }

    case LX_nanosleep:
        return do_sleep(LX_CLOCK_MONOTONIC, 0, (const struct lx_timespec *)a1,
                        (struct lx_timespec *)a2);
    case LX_clock_nanosleep: {
        /* This one returns its error rather than setting errno. */
        long r = do_sleep(a1, a2, (const struct lx_timespec *)a3, (struct lx_timespec *)a4);
        return r < 0 ? -r : r;
    }

    /* Refused deliberately, and each for a reason worth stating rather than
       leaving as an unexplained failure later:

       ioctl  — the terminal is a service on the other end of a descriptor,
                not a device with ioctls. ENOTTY is the true answer, and it is
                also the one that makes isatty() say "no" and stdio pick block
                buffering, which is what we want.
       rseq, robust lists, prlimit — no equivalent, and musl copes with
                being refused all three. */
    case LX_ioctl:
        /* Masked to thirty-two bits on the way in. A request is an `int` in
           the C library's signature, and the ones with the "read" bit set —
           `TIOCGPTN` is 0x80045430 — are negative in it, so they arrive here
           sign-extended and match nothing. */
        return __quark_ioctl(a1, (unsigned long)(unsigned int)a2, (unsigned long)a3);
    /* A deadline as a descriptor.
     *
     * The clock and the flags are read and not honoured: there is one clock
     * here, the tick, and a timerfd that is not close-on-exec is a
     * distinction this system does not draw. The resolution is ten
     * milliseconds, so anything asked for below that fires at the next tick —
     * which is the next time anything happens at all. */
    case LX_timerfd_create: {
        unsigned long fd = __syscall0(SYS_TIMER_CREATE);
        return fd == QUARK_ERR ? -LX_EMFILE : (long)fd;
    }
    case LX_timerfd_settime: {
        /* itimerspec: interval seconds and nanoseconds, then the same for the
           first expiration. Absolute deadlines (TFD_TIMER_ABSTIME) are turned
           into a delay here, since the kernel counts only forwards. */
        if (!a3) {
            return -LX_EINVAL;
        }
        const long *it = (const long *)a3;
        unsigned long interval = (unsigned long)(it[0] * 100 + it[1] / 10000000);
        unsigned long first_s = (unsigned long)it[2];
        unsigned long first_n = (unsigned long)it[3];
        unsigned long first = first_s * 100 + first_n / 10000000;
        if ((first_s || first_n) && first == 0) {
            first = 1; /* sooner than a tick is the next tick */
        }
        if (a2 & 1 /* TFD_TIMER_ABSTIME */) {
            unsigned long now = __syscall0(SYS_TICKS);
            first = first > now ? first - now : 1;
        }
        if (a4) {
            long *old = (long *)a4;
            unsigned long left = __syscall1(SYS_TIMER_GET, (unsigned long)a1);
            old[0] = (long)((left >> 32) / 100);
            old[1] = (long)(((left >> 32) % 100) * 10000000);
            old[2] = (long)((left & 0xFFFFFFFF) / 100);
            old[3] = (long)(((left & 0xFFFFFFFF) % 100) * 10000000);
        }
        return __syscall3(SYS_TIMER_SET, (unsigned long)a1, first, interval) == QUARK_ERR
                   ? -LX_EINVAL
                   : 0;
    }
    case LX_timerfd_gettime: {
        unsigned long left = __syscall1(SYS_TIMER_GET, (unsigned long)a1);
        if (left == QUARK_ERR) {
            return -LX_EINVAL;
        }
        if (a2) {
            long *out = (long *)a2;
            out[0] = (long)((left >> 32) / 100);
            out[1] = (long)(((left >> 32) % 100) * 10000000);
            out[2] = (long)((left & 0xFFFFFFFF) / 100);
            out[3] = (long)(((left & 0xFFFFFFFF) % 100) * 10000000);
        }
        return 0;
    }

    case LX_setsid:
        /* A session of the caller's own, and a group in it: what a terminal's
           child asks for before it asks for a controlling terminal. */
        return job_answer(__syscall1(SYS_PGROUP, QUARK_SESSION_NEW));
    /* Signals: signal.c. */
    case LX_rt_sigaction:
        return __quark_sigaction(a1, (const struct lx_ksigaction *)a2,
                                 (struct lx_ksigaction *)a3, (unsigned long)a4);
    case LX_rt_sigprocmask:
        return __quark_sigprocmask(a1, (const unsigned long *)a2, (unsigned long *)a3,
                                   (unsigned long)a4);
    case LX_rt_sigpending:
        return __quark_sigpending((unsigned long *)a1, (unsigned long)a2);
    case LX_rt_sigsuspend:
        return __quark_sigsuspend((const unsigned long *)a1, (unsigned long)a2);
    case LX_pause:
        return __quark_sigsuspend(NULL, 8);
    case LX_rt_sigtimedwait:
        return __quark_sigtimedwait((const unsigned long *)a1, (void *)a2, (const long *)a3,
                                    (unsigned long)a4);
    case LX_kill:
        return __quark_kill(a1, a2);
    case LX_tkill:
        return __quark_tkill(a1, a2);
    case LX_tgkill:
        return __quark_tkill(a2, a3);
    case LX_rt_sigreturn:
        /* Returning from a handler is returning from a function here. */
        return 0;
    /* The one interval timer there is: real time, and SIGALRM when it runs
       out. musl's `alarm` is a `setitimer`; the call of that name is here for
       a program that makes it itself. */
    case LX_alarm: {
        unsigned long was = __syscall3(SYS_SIG_ALARM, time_ticks((unsigned int)a1, 0), 0, 0);
        return was == QUARK_ERR ? 0 : (long)((ticks_left(was & 0xFFFFFFFFUL) + 99) / 100);
    }
    case LX_setitimer:
        return do_itimer(a1, (const long *)a2, (long *)a3);
    case LX_getitimer:
        return do_itimer(a1, NULL, (long *)a2);
    case LX_set_robust_list:
    case LX_rseq:
        return -LX_ENOSYS;

    /* Limits. Two are facts about this system — a program has sixty-four
       descriptors and a megabyte of stack — and the rest are not kept. */
    case LX_getrlimit:
        return do_getrlimit(a1, (unsigned long *)a2);
    case LX_setrlimit:
        return 0;
    case LX_prlimit64:
        if (a2 != 0 && a2 != __quark_getpid()) {
            return -LX_ESRCH;
        }
        return a4 ? do_getrlimit(a2 ? a1 : a1, (unsigned long *)a4) : 0;

    /* What a program has used is not counted. Zeroes are what `time` then
       prints, which is at least not a lie about a number nobody kept. */
    case LX_getrusage:
        if (a2) {
            unsigned char *usage = (unsigned char *)a2;
            for (int i = 0; i < 144; i++) {
                usage[i] = 0;
            }
        }
        return 0;
    case LX_times: {
        if (a1) {
            long *t = (long *)a1;
            t[0] = t[1] = t[2] = t[3] = 0;
        }
        /* Ticks since boot, at the hundred a second Linux counts in too. */
        return (long)__syscall0(SYS_TICKS);
    }
    case LX_sysinfo:
        return do_sysinfo((unsigned long *)a1);

    case LX_select: {
        const long *tv = (const long *)a5;
        return do_select(a1, (unsigned long *)a2, (unsigned long *)a3, (unsigned long *)a4,
                         tv ? tv[0] * 1000 + tv[1] / 1000 : -1);
    }
    case LX_pselect6: {
        const long *ts = (const long *)a5;
        long ms = ts ? ts[0] * 1000 + ts[1] / 1000000 : -1;
        /* The sixth argument is a pointer to a mask and its size. */
        const unsigned long *const *sixth = (const unsigned long *const *)a6;
        WAIT_UNDER(sixth ? sixth[0] : NULL,
                   do_select(a1, (unsigned long *)a2, (unsigned long *)a3, (unsigned long *)a4, ms));
    }

    /* The kernel's generator never blocks, so GRND_NONBLOCK, GRND_RANDOM
       and GRND_INSECURE all get the same answer. One kernel call gives at
       most a mebibyte; Linux gives at most this much in one of its own. */
    case LX_getrandom: {
        if ((unsigned long)a3 & ~7ul) {
            return -LX_EINVAL;
        }
        unsigned long want = (unsigned long)a2;
        if (want > 33554431ul) {
            want = 33554431ul;
        }
        unsigned long done = 0;
        while (done < want) {
            unsigned long n = __syscall2(SYS_GETRANDOM, (unsigned long)a1 + done, want - done);
            if (n == QUARK_ERR || n == 0) {
                return done ? (long)done : -LX_EFAULT;
            }
            done += n;
        }
        return (long)done;
    }

    case LX_uname:
        return do_uname((char *)a1);

    case LX_getcwd:
        return __quark_getcwd((char *)a1, (unsigned long)a2);
    case LX_chdir:
        return __quark_chdir((const char *)a1);
    case LX_fchdir:
        return __quark_fchdir(a1);

    case LX_getdents64:
        return __quark_getdents(a1, (void *)a2, (unsigned long)a3);
    case LX_readlink:
        return __quark_readlink(LX_AT_FDCWD, (const char *)a1, (char *)a2, (unsigned long)a3);
    case LX_readlinkat:
        return __quark_readlink(a1, (const char *)a2, (char *)a3, (unsigned long)a4);
    case LX_statfs:
        return __quark_statfs((const char *)a1, (void *)a2);
    case LX_fstatfs:
        return __quark_fstatfs(a1, (void *)a2);

    /* Files. The VFS answers all of these; `files.c` is where a handle and an
       offset become a descriptor. */
    case LX_open:
        return __quark_open((const char *)a1, a2, a3);
    case LX_openat:
        return __quark_openat(a1, (const char *)a2, a3, a4);
    case LX_close:
        return __quark_close(a1);
    case LX_lseek:
        return __quark_lseek(a1, a2, a3);
    case LX_fstat:
        return __quark_fstat(a1, (void *)a2);
    case LX_stat:
        return __quark_stat(LX_AT_FDCWD, (const char *)a1, (void *)a2, 1);
    case LX_lstat:
        return __quark_stat(LX_AT_FDCWD, (const char *)a1, (void *)a2, 0);
    case LX_newfstatat:
        /* AT_NO_AUTOMOUNT (0x800) asks for nothing here: nothing mounts. */
        if (a4 & ~(LX_AT_SYMLINK_NOFOLLOW | LX_AT_EMPTY_PATH | 0x800)) {
            return -LX_EINVAL;
        }
        if (!a2 || !*(const char *)a2) {
            if (!(a4 & LX_AT_EMPTY_PATH)) {
                return -LX_ENOENT;
            }
            /* The descriptor itself, which may be the working directory. */
            if (a1 == LX_AT_FDCWD) {
                return __quark_stat(LX_AT_FDCWD, ".", (void *)a3, 1);
            }
            return __quark_fstat(a1, (void *)a3);
        }
        return __quark_stat(a1, (const char *)a2, (void *)a3, !(a4 & LX_AT_SYMLINK_NOFOLLOW));

    /* access(2). gnulib's euidaccess tries faccessat2 first, then faccessat,
       and reports whatever the last one said — so refusing these is not a
       missing convenience: `sort /etc/passwd` says "cannot read" about a file
       it can read perfectly well. All three ask the same question, from the
       working directory or a directory descriptor. */
    case LX_access:
        return __quark_access(LX_AT_FDCWD, (const char *)a1, a2);
    case LX_faccessat:
    case LX_faccessat2:
        return __quark_access(a1, (const char *)a2, a3);

    case LX_fcntl:
        return __quark_fcntl(a1, a2, a3);
    case LX_flock:
        return __quark_flock(a1, a2);
    case LX_pread64:
        return __quark_pread(a1, (void *)a2, (unsigned long)a3, a4);
    case LX_pwrite64:
        return __quark_pwrite(a1, (const void *)a2, (unsigned long)a3, a4);
    case LX_dup:
        return __quark_dup(a1, -1);
    case LX_dup2:
        if (a2 < 0) {
            return -LX_EBADF;
        }
        return __quark_dup(a1, a2);
    /* dup3 differs in refusing a copy onto itself, and in the one flag it
       takes: the copy is closed when the program becomes another. */
    case LX_dup3: {
        if (a1 == a2 || (a3 & ~LX_O_CLOEXEC)) {
            return -LX_EINVAL;
        }
        if (a2 < 0) {
            return -LX_EBADF;
        }
        long copy = __quark_dup(a1, a2);
        if (copy >= 0 && (a3 & LX_O_CLOEXEC)) {
            __syscall3(SYS_FD_FLAGS, (unsigned long)copy, QUARK_FD_SETFLAGS, QUARK_FD_CLOEXEC);
        }
        return copy;
    }

    case LX_mkdir:
        return __quark_mkdir(LX_AT_FDCWD, (const char *)a1, a2);
    case LX_mkdirat:
        return __quark_mkdir(a1, (const char *)a2, a3);

    /* What a file's inode says of it. */
    case LX_umask:
        return __quark_umask(a1);
    case LX_chmod:
        return __quark_chmod(LX_AT_FDCWD, (const char *)a1, a2, 0);
    case LX_fchmod:
        return __quark_chmod(a1, NULL, a2, 0);
    case LX_fchmodat:
        return __quark_chmod(a1, (const char *)a2, a3, 0);
    case LX_fchmodat2:
        if (a4 & ~(LX_AT_SYMLINK_NOFOLLOW | LX_AT_EMPTY_PATH)) {
            return -LX_EINVAL;
        }
        return __quark_chmod(a1, (a4 & LX_AT_EMPTY_PATH) && !*(const char *)a2 ? NULL
                                                                                : (const char *)a2,
                             a3, (a4 & LX_AT_SYMLINK_NOFOLLOW) != 0);
    case LX_chown:
        return __quark_chown(LX_AT_FDCWD, (const char *)a1, a2, a3, 0);
    case LX_lchown:
        return __quark_chown(LX_AT_FDCWD, (const char *)a1, a2, a3, 1);
    case LX_fchown:
        return __quark_chown(a1, NULL, a2, a3, 0);
    case LX_fchownat:
        if (a5 & ~(LX_AT_SYMLINK_NOFOLLOW | LX_AT_EMPTY_PATH)) {
            return -LX_EINVAL;
        }
        return __quark_chown(a1, (a5 & LX_AT_EMPTY_PATH) && !*(const char *)a2 ? NULL
                                                                                : (const char *)a2,
                             a3, a4, (a5 & LX_AT_SYMLINK_NOFOLLOW) != 0);
    /* A NULL path is `futimens`: the descriptor itself. */
    case LX_utimensat:
        if (a4 & ~LX_AT_SYMLINK_NOFOLLOW) {
            return -LX_EINVAL;
        }
        return __quark_utimens(a1, (const char *)a2, (const long *)a3,
                               (a4 & LX_AT_SYMLINK_NOFOLLOW) != 0);

    /* A write is answered before the filesystem has recorded it for good:
       its data is on the disk, and what says the file is that long waits a
       moment for the next write to say so too. These wait for it. They do
       not tell one file from another, or one filesystem: everything is
       recorded, which is more than was asked and never less. */
    case LX_fsync:
    case LX_fdatasync:
    case LX_syncfs:
        return __quark_fsync(a1);
    case LX_sync:
        return __quark_fsync(-1);

    /* No extended attributes. "Not supported" is the answer that makes `ls`
       and `cp` carry on as for a filesystem that has none; "no such call"
       makes them complain about every file. */
    case LX_getxattr:
    case LX_lgetxattr:
    case LX_fgetxattr:
    case LX_listxattr:
    case LX_llistxattr:
    case LX_flistxattr:
    case LX_setxattr:
    case LX_lsetxattr:
    case LX_fsetxattr:
    case LX_removexattr:
    case LX_lremovexattr:
    case LX_fremovexattr:
        return -LX_EOPNOTSUPP;

    /* A named pipe can be made, and a device cannot. */
    case LX_mknod:
        return __quark_mknodat(LX_AT_FDCWD, (const char *)a1, a2);
    case LX_mknodat:
        return __quark_mknodat(a1, (const char *)a2, a3);

    case LX_unlink:
        return __quark_unlink(LX_AT_FDCWD, (const char *)a1);
    case LX_rmdir:
        return __quark_rmdir(LX_AT_FDCWD, (const char *)a1);
    case LX_unlinkat:
        if (a3 & ~LX_AT_REMOVEDIR) {
            return -LX_EINVAL;
        }
        return (a3 & LX_AT_REMOVEDIR) ? __quark_rmdir(a1, (const char *)a2)
                                       : __quark_unlink(a1, (const char *)a2);
    case LX_rename:
        return __quark_rename(LX_AT_FDCWD, (const char *)a1, LX_AT_FDCWD, (const char *)a2);
    case LX_renameat:
    case LX_renameat2:
        /* renameat2's flags (no-replace, exchange) are not offered. */
        if (n == LX_renameat2 && a5 != 0) {
            return -LX_EINVAL;
        }
        return __quark_rename(a1, (const char *)a2, a3, (const char *)a4);
    case LX_link:
        return __quark_link(LX_AT_FDCWD, (const char *)a1, LX_AT_FDCWD, (const char *)a2, 0);
    case LX_linkat:
        /* Naming the source by descriptor (AT_EMPTY_PATH) is not offered. */
        if (a5 & ~LX_AT_SYMLINK_FOLLOW) {
            return -LX_EINVAL;
        }
        return __quark_link(a1, (const char *)a2, a3, (const char *)a4,
                            (a5 & LX_AT_SYMLINK_FOLLOW) != 0);
    case LX_symlink:
        return __quark_symlink((const char *)a1, LX_AT_FDCWD, (const char *)a2);
    case LX_symlinkat:
        return __quark_symlink((const char *)a1, a2, (const char *)a3);
    case LX_truncate:
        return __quark_truncate((const char *)a1, a2);

    /* Streams, descriptors in flight, and waiting. Each is the same operation
       Quark has with Linux's packaging around it. */
    case LX_memfd_create:
        return __quark_memfd((const char *)a1, a2);
    case LX_ftruncate:
        if (__quark_fd_is_file(a1)) {
            return __quark_file_truncate(a1, a2);
        }
        return __quark_ftruncate(a1, a2);
    case LX_fallocate:
        /* posix_fallocate(fd, offset, len) is what musl uses when it has it,
           and libwayland's os_create_anonymous_file prefers it to ftruncate.
           There is nothing to preallocate here -- a region's frames are taken
           when it is sized -- so the size is all of it. For a file it is
           the length: one shorter than was asked for is made that long, and
           the blocks come when they are written. */
        if (__quark_fd_is_file(a1)) {
            struct { unsigned long w[18]; } st;
            long err = __quark_fstat(a1, &st);
            if (err) {
                return err;
            }
            /* st_size is the seventh word of the kernel's stat. */
            return (long)st.w[6] >= a3 + a4 ? 0 : __quark_file_truncate(a1, a3 + a4);
        }
        return __quark_ftruncate(a1, a3 + a4);
    case LX_eventfd:
    case LX_eventfd2: {
        /* A counter with a descriptor. glib reaches for this before anything
           else to wake a sleeping main loop, falling back to a pipe only when
           it fails — and so do libwayland's loop and GTK's. Two of the flags
           are ours to act on: EFD_NONBLOCK, which the read path reads, and
           EFD_SEMAPHORE, which the kernel's counter implements. EFD_CLOEXEC
           means nothing here, as it does for a pipe. */
        long flags = (n == LX_eventfd2) ? a2 : 0;
        unsigned long fd = __syscall2(SYS_EVENT_CREATE, (unsigned long)a1,
                                      (flags & LX_EFD_SEMAPHORE) ? 1UL : 0UL);
        if (fd == QUARK_ERR) {
            return -LX_EMFILE;
        }
        if (flags & LX_EFD_NONBLOCK) {
            __quark_fd_set_nonblock((long)fd, 1);
        }
        return (long)fd;
    }
    case LX_pipe:
        return __quark_pipe((int *)a1, 0);
    case LX_pipe2:
        return __quark_pipe((int *)a1, a2);
    case LX_socketpair:
        return __quark_socketpair(a1, a2, a3, (int *)a4);
    /* A socket by itself, to be bound or connected by a name: there are
       none, of any family. Said as "that family is not supported", which is
       the answer a program has something to do about — the C library asks a
       name service daemon who a user is before it concludes there is no
       such user, takes this for "there is no daemon", and reports nobody
       found. Told "no such call" it reported that instead, and `id nobody`
       said the function was not implemented. */
    case LX_socket:
        return -LX_EAFNOSUPPORT;
    case LX_sendmsg:
        return __quark_sendmsg(a1, (const void *)a2, a3);
    case LX_recvmsg:
        return __quark_recvmsg(a1, (void *)a2, a3);
    case LX_poll:
        return __quark_poll((void *)a1, a2, a3);
    case LX_ppoll: {
        /* A timespec rather than milliseconds, and a mask to wait under. */
        const long *ts = (const long *)a3;
        long ms = ts ? ts[0] * 1000 + ts[1] / 1000000 : -1;
        WAIT_UNDER((const unsigned long *)a4, __quark_poll((void *)a1, a2, ms));
    }
    case LX_epoll_pwait:
        WAIT_UNDER((const unsigned long *)a5, __quark_epoll_wait(a1, (void *)a2, a3, a4));
    case LX_epoll_create1:
        return __quark_epoll_create();
    case LX_epoll_ctl:
        return __quark_epoll_ctl(a1, a2, a3, (void *)a4);
    case LX_epoll_wait:
        return __quark_epoll_wait(a1, (void *)a2, a3, a4);

    /* statx carries more than the VFS knows, and musl falls back to the plain
       stat calls when it is refused. */
    case LX_statx:
        return -LX_ENOSYS;

    default:
        trace("nosys", n);
        return -LX_ENOSYS;
    }
}
