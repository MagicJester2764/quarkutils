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
#define LX_reboot          169
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
#define LX_rt_sigqueueinfo 129
#define LX_rt_tgsigqueueinfo 297
#define LX_timer_create    222
#define LX_timer_settime   223
#define LX_timer_gettime   224
#define LX_timer_getoverrun 225
#define LX_timer_delete    226
#define LX_signalfd        282
#define LX_signalfd4       289
#define LX_inotify_init    253
#define LX_inotify_add_watch 254
#define LX_inotify_rm_watch  255
#define LX_inotify_init1   294
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
#define LX_shutdown         48
#define LX_socket           41
#define LX_connect          42
#define LX_accept           43
#define LX_sendto           44
#define LX_recvfrom         45
#define LX_bind             49
#define LX_listen           50
#define LX_getsockname      51
#define LX_getpeername      52
#define LX_setsockopt       54
#define LX_getsockopt       55
#define LX_accept4          288
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
#define LX_prctl           157
#define LX_mremap          25
#define LX_get_robust_list 274
#define LX_sched_getaffinity 204
#define LX_getcpu          309
#define LX_futex           202
#define LX_gettid          186
#define LX_set_tid_address 218
#define LX_clock_settime   227
#define LX_clock_gettime   228
#define LX_clock_getres    229
#define LX_gettimeofday     96
#define LX_settimeofday    164
#define LX_time            201
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
/* Where the next mapping goes; nought until the arena is first used, when it
   begins a random number of pages into its first quarter (`arena_next`). */
static unsigned long mmap_next;
/* Choosing an address, mapping there and stepping past it are three steps,
   and every thread of a program allocates. Two that chose the same address
   had one mapping refused — the kernel does not map over what is there —
   and the program was told there was no memory: a keymap that could not be
   read, a `malloc` that came back null, on a machine with a gigabyte free.
   One thread at a time is in the arena. */
static int arena_lock;

/* The lock: 0 free, 1 held, 2 held and somebody is waiting on it. A futex,
   so that waiting is the kernel's and a holder that is not running is not
   spun at. */
void __quark_lock(int *lock) {
    int seen = 0;
    if (__atomic_compare_exchange_n(lock, &seen, 1, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) {
        return;
    }
    if (seen != 2) {
        seen = __atomic_exchange_n(lock, 2, __ATOMIC_ACQUIRE);
    }
    while (seen != 0) {
        __syscall2(SYS_FUTEX_WAIT, (unsigned long)lock, 2);
        seen = __atomic_exchange_n(lock, 2, __ATOMIC_ACQUIRE);
    }
}

void __quark_unlock(int *lock) {
    if (__atomic_exchange_n(lock, 0, __ATOMIC_RELEASE) == 2) {
        __syscall2(SYS_FUTEX_WAKE, (unsigned long)lock, 1);
    }
}

void __quark_arena_forked(void) {
    arena_lock = 0;
}

unsigned long __quark_random_pages(unsigned long window) {
    unsigned long chance = 0;
    if (window == 0 || __syscall2(SYS_GETRANDOM, (unsigned long)&chance, sizeof chance) != sizeof chance) {
        return 0;
    }
    return chance % window;
}

#define PAGE_SIZE 4096UL
/* What sys_munmap will take in one call. */
#define MAP_CHUNK 256UL

/* Where the next mapping goes, with the arena's lock held. */
static unsigned long arena_next(void) {
    if (mmap_next == 0) {
        mmap_next = MMAP_BASE + __quark_random_pages((MMAP_LIMIT - MMAP_BASE) / 4 / PAGE_SIZE) * PAGE_SIZE;
    }
    return mmap_next;
}

/* A line every two gigabytes, which a mapping smaller than that does not
   cross. pixman's stress test reaches an image through its address with
   bits 63 and 31 turned over, adds the offset to that and turns them back,
   which is the address only while the image stays on one side of such a
   line. On Linux a buffer seldom crosses one; here, where the arena only
   goes up — from a random start, and in steps of gigabytes when the test
   asks for its masks — one did, now and then, and the test read four
   gigabytes from where it meant to. */
#define ARENA_LINE (1UL << 31)

/* Where a mapping of `pages` goes: the next address, or the next line if it
   would cross one and need not. With the arena's lock held. */
static unsigned long arena_place(unsigned long pages) {
    unsigned long at = arena_next();
    unsigned long len = pages * PAGE_SIZE;
    if (len < ARENA_LINE && at / ARENA_LINE != (at + len - 1) / ARENA_LINE) {
        at = (at + ARENA_LINE - 1) & ~(ARENA_LINE - 1);
    }
    return at;
}

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

#define LX_MAP_FIXED           0x10
#define LX_MAP_FIXED_NOREPLACE 0x100000

/* Where a mapping goes, with the arena's lock held: where the program said,
   for one it places itself, or else where the arena says. A dynamic loader
   places itself every segment of a library after the first, inside the span
   the first took, and that is what MAP_FIXED is for: whatever was in the
   range goes, as on Linux — MAP_FIXED_NOREPLACE says it must have been
   empty, and the kernel, which never maps over anything, says whether it
   was. 0 for an address no mapping can be at. */
static unsigned long place(unsigned long hint, unsigned long pages, long flags) {
    if (!(flags & (LX_MAP_FIXED | LX_MAP_FIXED_NOREPLACE))) {
        unsigned long at = arena_place(pages);
        return at < MMAP_LIMIT && pages <= (MMAP_LIMIT - at) / PAGE_SIZE ? at : 0;
    }
    if ((hint & (PAGE_SIZE - 1)) || hint < QUARK_USER_MIN || pages > (QUARK_USER_END - hint) / PAGE_SIZE) {
        return 0;
    }
    if (flags & LX_MAP_FIXED) {
        unmap_pages(hint, pages);
    }
    return hint;
}

/* The arena after a mapping at `at`: past it, if it reaches beyond where the
   next would go — and never back, since a mapping a program placed itself
   may be below where the arena has got to. */
static void placed(unsigned long at, unsigned long pages) {
    unsigned long end = at + pages * PAGE_SIZE;
    if (end > arena_next() && at < MMAP_LIMIT) {
        mmap_next = end;
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
static long do_mmap(unsigned long hint, unsigned long len, long flags) {
    if (len == 0) {
        return -LX_EINVAL;
    }
    unsigned long pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    __quark_lock(&arena_lock);
    unsigned long at = place(hint, pages, flags);
    if (at == 0) {
        __quark_unlock(&arena_lock);
        trace("mmap-arena-full", (long)pages);
        return (flags & (LX_MAP_FIXED | LX_MAP_FIXED_NOREPLACE)) ? -LX_EINVAL : -LX_ENOMEM;
    }
    unsigned long how = (flags & LX_MAP_POPULATE) ? QUARK_MAP_POPULATE : 0;
    if (!(flags & LX_MAP_NORESERVE)) {
        how |= QUARK_MAP_ACCOUNT;
    }
    if (__syscall3(SYS_MAP_ANON, at, pages, how) == QUARK_ERR) {
        __quark_unlock(&arena_lock);
        trace("mmap-failed-pages", (long)pages);
        return (flags & LX_MAP_FIXED_NOREPLACE) ? -LX_EEXIST : -LX_ENOMEM;
    }
    placed(at, pages);
    __quark_unlock(&arena_lock);
    return (long)at;
}

/* Map memory named by a descriptor, at an address of our choosing. */
static long do_mmap_fd(long fd, unsigned long hint, unsigned long len, long flags) {
    if (len == 0) {
        return -LX_EINVAL;
    }
    unsigned long pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    __quark_lock(&arena_lock);
    unsigned long at = place(hint, pages, flags);
    if (at == 0) {
        __quark_unlock(&arena_lock);
        return (flags & (LX_MAP_FIXED | LX_MAP_FIXED_NOREPLACE)) ? -LX_EINVAL : -LX_ENOMEM;
    }
    /* The whole region is mapped, whatever the caller asked to see of it --
       Quark has no partial mapping of a descriptor. The call says how much
       that was, and the arena must step over all of it: advancing by the
       caller's length instead would hand the next mapping addresses that are
       already occupied, and the kernel refuses to map over them. */
    unsigned long got = __syscall2(SYS_MMAP_FD, (unsigned long)fd, at);
    if (got == QUARK_ERR) {
        __quark_unlock(&arena_lock);
        return -LX_ENODEV;
    }
    unsigned long real = (got + PAGE_SIZE - 1) / PAGE_SIZE;
    placed(at, real > pages ? real : pages);
    __quark_unlock(&arena_lock);
    return (long)at;
}

#define LX_PROT_WRITE   2
#define LX_PROT_EXEC    4
#define LX_MAP_SHARED   1

/* A file, through the VFS: a memory object whose pages the server provides
   as they are touched. The capability it grants is only needed to make the
   mapping, which keeps the object; it goes straight after. */
static long do_mmap_file(long fd, unsigned long hint, unsigned long len, long prot, long flags, long off) {
    if (len == 0 || off < 0 || (off & (PAGE_SIZE - 1))) {
        return -LX_EINVAL;
    }
    unsigned long pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    int shared = (flags & LX_MAP_SHARED) != 0;
    int write = (prot & LX_PROT_WRITE) != 0;
    /* The server is asked first, and outside the arena's lock: it is a call
       to another program, which may take as long as a disk does. */
    unsigned long slot;
    long bad = __quark_file_map(fd, shared && write, &slot);
    if (bad) {
        return bad;
    }
    unsigned long how = (write ? QUARK_OBJECT_WRITE : 0) | (shared ? QUARK_OBJECT_SHARED : 0) |
                        ((prot & LX_PROT_EXEC) ? QUARK_OBJECT_EXEC : 0);
    __quark_lock(&arena_lock);
    unsigned long at = place(hint, pages, flags);
    unsigned long r = QUARK_ERR;
    if (at != 0) {
        r = __syscall5(SYS_OBJECT_MAP, slot, at, pages, (unsigned long)off / PAGE_SIZE, how);
    }
    if (r != QUARK_ERR) {
        placed(at, pages);
    }
    __quark_unlock(&arena_lock);
    __syscall1(SYS_CAP_DELETE, slot);
    if (r != QUARK_ERR) {
        return (long)at;
    }
    if (at == 0) {
        return (flags & (LX_MAP_FIXED | LX_MAP_FIXED_NOREPLACE)) ? -LX_EINVAL : -LX_ENOMEM;
    }
    return (flags & LX_MAP_FIXED_NOREPLACE) ? -LX_EEXIST : -LX_ENOMEM;
}

/* The first thread's stack: its top, the page above the one the arguments
   are on, where the environment musl was handed still is when the constructor below runs.
   Every loader here makes it 256 pages (`quark_rt::spawn`, process.c). */
extern char **__environ;
static unsigned long first_stack_top;
#define FIRST_STACK_PAGES 256UL

/* Anonymous memory that is shared: memory named by a descriptor nobody
   keeps, mapped, so that a forked child has these pages and not a copy of
   them — which is what MAP_SHARED|MAP_ANONYMOUS is for: a lock, a
   condition, a count a parent and its children all see. */
static long do_mmap_shared(unsigned long hint, unsigned long len, long flags) {
    if (len == 0) {
        return -LX_EINVAL;
    }
    unsigned long pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    unsigned long fd = __syscall1(SYS_MEMFD_CREATE, pages);
    if (fd == QUARK_ERR) {
        return -LX_ENOMEM;
    }
    long at = do_mmap_fd((long)fd, hint, len, flags);
    __syscall1(SYS_FD_CLOSE, fd);
    return at == -LX_ENODEV ? -LX_ENOMEM : at;
}

#define LX_MREMAP_MAYMOVE 1
#define LX_MREMAP_FIXED   2

/* mremap, for the one question asked of it here that has an answer: whether
   a mapping can grow where it is. That is how musl finds how far the first
   thread's stack goes (pthread_getattr_np) — page by page down from its
   top, until the page it names is not there. Nothing grows in place here:
   a page of the first stack is ENOMEM, it cannot, and any other EFAULT, as
   for a page that is not there. Nothing is moved, either (MREMAP_MAYMOVE
   is ENOMEM), and musl's realloc copies instead. */
static long do_mremap(unsigned long old, unsigned long new_len, long flags) {
    if ((old & (PAGE_SIZE - 1)) || new_len == 0 || (flags & ~(long)(LX_MREMAP_MAYMOVE | LX_MREMAP_FIXED))) {
        return -LX_EINVAL;
    }
    if (flags & LX_MREMAP_MAYMOVE) {
        return -LX_ENOMEM;
    }
    unsigned long bottom = first_stack_top - FIRST_STACK_PAGES * PAGE_SIZE;
    return first_stack_top && old >= bottom && old < first_stack_top ? -LX_ENOMEM : -LX_EFAULT;
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

/* Which timer descriptors were made on the clock that says the date: a bit
   for each descriptor. */
static unsigned long timer_wall;

/* Is `clock` one that says the date, rather than how long the machine has
   been on? */
static int wall_clock(long clock) {
    return clock == LX_CLOCK_REALTIME || clock == LX_CLOCK_REALTIME_COARSE || clock == LX_CLOCK_TAI;
}

/* Sleep until `req` has passed on `clock` — or, with TIMER_ABSTIME, until
   the clock reads `req`. The kernel keeps the time to the nanosecond and
   never ends a wait early, so what is asked for is what is waited. It waits
   by receiving from itself, which nobody sends to.

   A time on the clock that says the date is turned into how long from now
   when the sleep begins: the date being set meanwhile does not move it. */
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
    unsigned long now = quark_now();
    unsigned long asked = quark_nanos((unsigned long)req->tv_sec, (unsigned long)req->tv_nsec);
    unsigned long deadline;
    if (flags & LX_TIMER_ABSTIME) {
        unsigned long reads = wall_clock(clock) ? __syscall1(SYS_CLOCK, QUARK_CLOCK_WALL) : now;
        if (asked <= reads) {
            return 0;
        }
        deadline = now + (asked - reads);
    } else {
        deadline = now + asked;
    }
    if (deadline < now) {
        deadline = ~0UL;
    }
    while ((now = quark_now()) < deadline) {
        /* A wait for no signal at all, which a signal with a handler ends. */
        unsigned long r = __syscall3(SYS_SIG_WAIT, 0, quark_span(deadline - now), 0);
        /* A signal ended the sleep. If a handler ran, that is the sleep over,
           with what was left of it said — a sleep is never taken up again,
           whatever the handler asked for. If none did, the signal is blocked
           or was not this thread's to take, and there is the rest of the
           sleep still to do. */
        if (quark_cut_short(r, 0) < 0) {
            if (rem && !(flags & LX_TIMER_ABSTIME)) {
                now = quark_now();
                unsigned long left = now < deadline ? deadline - now : 0;
                rem->tv_sec = (long)(left / 1000000000UL);
                rem->tv_nsec = (long)(left % 1000000000UL);
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

#define LX_RLIMIT_CPU    0
#define LX_RLIMIT_STACK  3
#define LX_RLIMIT_NOFILE 7
#define LX_RLIMIT_SIGPENDING 11
#define LX_RLIM_INFINITY (~0UL)

static long do_getrlimit(long what, unsigned long *lim) {
    if (!lim) {
        return -LX_EFAULT;
    }
    if (what == LX_RLIMIT_CPU) {
        /* The kernel's: seconds of processor time, soft and hard. */
        lim[0] = lim[1] = LX_RLIM_INFINITY;
        __syscall4(SYS_CPU_LIMIT, 0, 0, (unsigned long)lim, 1);
        return 0;
    }
    unsigned long v;
    switch (what) {
    case LX_RLIMIT_NOFILE: v = 64; break;           /* the kernel's table */
    case LX_RLIMIT_STACK:  v = 256 * 4096UL; break; /* what a spawner gives */
    /* How many signals may wait behind their first, which is what sysconf
       answers _SC_SIGQUEUE_MAX with: the kernel's room for a program. */
    case LX_RLIMIT_SIGPENDING: v = 64; break;
    default:               v = LX_RLIM_INFINITY; break;
    }
    lim[0] = v; /* soft */
    lim[1] = v; /* hard */
    return 0;
}

static long do_setrlimit(long what, const unsigned long *lim) {
    if (!lim) {
        return -LX_EFAULT;
    }
    if (what != LX_RLIMIT_CPU) {
        /* Not kept: a program that lowers one is told it did. */
        return 0;
    }
    if (lim[0] > lim[1]) {
        return -LX_EINVAL;
    }
    unsigned long r = __syscall4(SYS_CPU_LIMIT, lim[0], lim[1], 0, 0);
    return r == 0 ? 0 : r == QUARK_NOT_ALLOWED ? -LX_EPERM : -LX_EINVAL;
}

/* What was used, as SYS_USAGE says it: nanoseconds in the program and in the
   kernel for it, and how many times it gave the processor up and had it
   taken. Whose: 0 this program, 1 the children it collected, 2 this thread. */
static void usage_of(unsigned long whose, unsigned long u[4]) {
    if (__syscall2(SYS_USAGE, whose, (unsigned long)u) == QUARK_ERR) {
        u[0] = u[1] = u[2] = u[3] = 0;
    }
}

/* A clock of a thread's or a program's processor time, as Linux numbers
   one (pthread_getcpuclockid, clock_getcpuclockid): negative, a task or a
   process id above three bits that say which — bit 2 a thread's. A thread's
   is any thread of this program's; a program's, this one's. What it has
   used, into `u`; or EINVAL. */
static long cpu_clock(long id, unsigned long u[4]) {
    long who = ~(id >> 3);
    if ((id & 3) == 3 || who <= 0) {
        return -LX_EINVAL;
    }
    unsigned long mine = __syscall1(SYS_PID, 0);
    if (id & 4) {
        if (__syscall1(SYS_PID, (unsigned long)who) != mine ||
            __syscall3(SYS_USAGE, 4, (unsigned long)u, (unsigned long)who) == QUARK_ERR) {
            return -LX_EINVAL;
        }
        return 0;
    }
    if ((unsigned long)who != mine) {
        return -LX_EINVAL;
    }
    usage_of(0, u);
    return 0;
}

#define LX_PR_SET_NAME 15
#define LX_PR_GET_NAME 16

/* What this thread is called: PR_SET_NAME and PR_GET_NAME, which is how
   musl names a thread for itself (pthread_setname_np); one it has not named
   is called what its program is. Any other option is as it was: not here. */
static long prctl(long option, long arg) {
    char *name = (char *)arg;
    unsigned long me = __syscall0(SYS_GETPID);
    if (option == LX_PR_SET_NAME) {
        if (!name) {
            return -LX_EFAULT;
        }
        unsigned long n = 0;
        while (n < 15 && name[n]) {
            n++;
        }
        return __syscall5(SYS_TASK_NAME, me, 0, (unsigned long)name, n, 0) == QUARK_ERR ? -LX_EINVAL : 0;
    }
    if (option == LX_PR_GET_NAME) {
        if (!name) {
            return -LX_EFAULT;
        }
        unsigned long n = __syscall4(SYS_TASK_NAME, me, 1, (unsigned long)name, 15);
        if (n == QUARK_ERR || n == 0) {
            /* Its program's: the last part of what it was started as. */
            char line[128];
            unsigned long len = __syscall4(SYS_PROGRAM_NAME, me, 1, (unsigned long)line, sizeof line - 1);
            if (len == QUARK_ERR) {
                len = 0;
            }
            line[len] = 0;
            const char *base = line;
            for (const char *c = line; *c; c++) {
                if (*c == '/') {
                    base = c + 1;
                }
            }
            n = 0;
            while (n < 15 && base[n]) {
                name[n] = base[n];
                n++;
            }
            if (n >= 4 && name[n - 4] == '.' && name[n - 3] == 'E' && name[n - 2] == 'L' && name[n - 1] == 'F') {
                n -= 4;
            }
        }
        name[n < 16 ? n : 15] = 0;
        return 0;
    }
    return -LX_ENOSYS;
}

/* That, as a struct rusage: two timevals and fourteen counts, of which the
   last two are the switches. */
static void fill_rusage(void *out, const unsigned long u[4]) {
    long *w = out;
    for (int i = 0; i < 18; i++) {
        w[i] = 0;
    }
    w[0] = (long)(u[0] / 1000000000UL);
    w[1] = (long)(u[0] % 1000000000UL / 1000);
    w[2] = (long)(u[1] / 1000000000UL);
    w[3] = (long)(u[1] % 1000000000UL / 1000);
    w[16] = (long)u[2];
    w[17] = (long)u[3];
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

/* How long a wait is, as `__quark_poll` and `__quark_epoll_wait` take it:
   nanoseconds, and for ever when it is negative. From seconds and
   nanoseconds, which are never for ever; and from the milliseconds `poll`
   and `epoll_wait` say it in, which are when they are negative. */
static long wait_ns(long sec, long nsec) {
    if (sec < 0 || nsec < 0) {
        return 0;
    }
    return (long)quark_nanos((unsigned long)sec, (unsigned long)nsec);
}

static long wait_ms(int ms) {
    return ms < 0 ? -1 : (long)ms * 1000000L;
}

/* select, over the same wait poll uses. An fd_set is an array of words, one
   bit a descriptor. */
static long do_select(long nfds, unsigned long *rd, unsigned long *wr, unsigned long *ex,
                      long timeout_ns, const unsigned long *under) {
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
    long got = __quark_poll(p, n, timeout_ns, under);
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

/* Set the program's alarm for `first` nanoseconds from now and every
   `every` after that — or, with `ask`, leave it — and say how it stood, in
   nanoseconds: what was left of it, and what it repeated at. No time at all
   is no alarm, which is how one is turned off. */
static unsigned long do_alarm(unsigned long first, unsigned long every, int ask, unsigned long was[2]) {
    was[0] = was[1] = 0;
    return __syscall4(SYS_SIG_ALARM, first ? quark_span(first) : 0, every ? quark_span(every) : 0,
                      ask ? QUARK_ALARM_ASK : 0, (unsigned long)was);
}

/* setitimer and getitimer. An itimerval is two timevals, the interval and
   then what is left, each seconds and microseconds.

   ITIMER_REAL is the kernel's alarm. The other two count the time a program
   spends running, which nothing here measures. */
static long do_itimer(long which, const long *set, long *old) {
    if (which != 0 /* ITIMER_REAL */) {
        return -LX_EINVAL;
    }
    unsigned long was[2], r;
    if (set) {
        if (set[0] < 0 || set[2] < 0 || set[1] < 0 || set[1] >= 1000000 || set[3] < 0
            || set[3] >= 1000000) {
            return -LX_EINVAL;
        }
        r = do_alarm(quark_nanos((unsigned long)set[2], (unsigned long)set[3] * 1000UL),
                     quark_nanos((unsigned long)set[0], (unsigned long)set[1] * 1000UL), 0, was);
    } else {
        r = do_alarm(0, 0, 1, was);
    }
    if (r == QUARK_ERR) {
        return -LX_EINVAL;
    }
    if (old) {
        /* In microseconds, rounded up: time still to come is never said to
           be none. */
        unsigned long left = (was[0] + 999) / 1000, every = (was[1] + 999) / 1000;
        old[0] = (long)(every / 1000000);
        old[1] = (long)(every % 1000000);
        old[2] = (long)(left / 1000000);
        old[3] = (long)(left % 1000000);
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

static long dispatch(long n, long a1, long a2, long a3, long a4, long a5, long a6);

long __quark_syscall(long n, long a1, long a2, long a3, long a4, long a5, long a6);

void __quark_sig_start(void);
void __quark_linux_start(void);

/* What a program's loader said of it under `key`, from the auxiliary vector
   that follows its environment on the stack it began with — before main, so
   before anything could have moved the environment. 0 if it said nothing. */
static unsigned long told(unsigned long key) {
    char **e = __environ;
    while (e && *e) {
        e++;
    }
    for (const unsigned long *aux = (const unsigned long *)(e + 1); e && aux[0] != 0; aux += 2) {
        if (aux[0] == key) {
            return aux[1];
        }
    }
    return 0;
}

/* Where the kernel enters this program to run a signal's handler, and how a
   call a signal cuts short is to be answered — before main, and before any
   constructor of the program's own. A C library's entry reads what Linux
   leaves on the stack, which is what every loader here leaves there now,
   and calls nothing of Quark's; this file is in every C program, since every
   call musl makes is made through it, so this runs in every one. A program
   built when the entry built its own arguments (`__quark_start_args`) says
   it twice, which is the same as once. */
__attribute__((constructor(101))) static void quark_start(void) {
    __quark_sig_start();
    first_stack_top = ((unsigned long)__environ + 4095UL) & ~4095UL;
    /* And a program built for Linux: the calls its own code makes are
       answered here too. */
    if (told(QUARK_AT_LINUX)) {
        __quark_linux_start();
    }
}

/* Every call musl makes arrives here. A handler runs wherever the kernel
   finds the program, on the way out of a call or an interrupt: nothing is
   left for this to do about one. */
long __quark_syscall(long n, long a1, long a2, long a3, long a4, long a5, long a6) {
    return dispatch(n, a1, a2, a3, a4, a5, a6);
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
            return do_mmap_file(a5, (unsigned long)a1, (unsigned long)a2, a3, a4, a6);
        }
        if (a5 != -1L) {
            return do_mmap_fd(a5, (unsigned long)a1, (unsigned long)a2, a4);
        }
        if ((a4 & 3) == 1 || (a4 & 3) == 3) {
            return do_mmap_shared((unsigned long)a1, (unsigned long)a2, a4);
        }
        return do_mmap((unsigned long)a1, (unsigned long)a2, a4);

    case LX_mremap:
        return do_mremap((unsigned long)a1, (unsigned long)a3, a4);

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
        /* What the child used is what the children collected used, after
           this, less what they had before it. */
        unsigned long before[4] = {0, 0, 0, 0};
        if (a4) {
            usage_of(1, before);
        }
        unsigned long got;
        for (;;) {
            got = __syscall2(SYS_WAIT_FOR, who, how);
            /* A signal ended the wait: made again, or EINTR, as the handler
               asked. Looked at before the answer is read as a child's: its
               shape is a report's. */
            long cut = quark_cut_short(got, 1);
            if (cut < 0) {
                return cut;
            }
            if (!cut) {
                break;
            }
        }
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
        if (a4) {
            unsigned long after[4];
            usage_of(1, after);
            for (int i = 0; i < 4; i++) {
                after[i] -= before[i];
            }
            fill_rusage((void *)a4, after);
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
    /* How nice a process is, which the kernel keeps for the program. The raw
       call answers 20 less it, as Linux's does, and the C library takes it
       back. One process, or this one for a group or a user of nought. */
    case LX_getpriority: {
        if (a1 != 0 && a2 != 0) {
            return -LX_EINVAL;
        }
        unsigned long r = __syscall2(SYS_NICE, a1 == 0 ? (unsigned long)a2 : 0, QUARK_ERR);
        return r > 39 ? -LX_ESRCH : 40 - (long)r;
    }
    case LX_setpriority: {
        if (a1 != 0 && a2 != 0) {
            return -LX_EINVAL;
        }
        long nice = a3 < -20 ? -20 : a3 > 19 ? 19 : a3;
        unsigned long r = __syscall2(SYS_NICE, a1 == 0 ? (unsigned long)a2 : 0, (unsigned long)nice);
        return r == QUARK_NOT_ALLOWED ? -LX_EACCES : r > 39 ? -LX_ESRCH : 0;
    }
    case LX_sched_yield:
        __syscall0(SYS_YIELD);
        return 0;
    case LX_sigaltstack:
        return __quark_sigaltstack((const void *)a1, (void *)a2);

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
        /* FUTEX_WAIT_BITSET and FUTEX_WAKE_BITSET — what Rust's standard
           library waits and wakes with — are FUTEX_WAIT and FUTEX_WAKE but
           for two things. The time is a deadline, on CLOCK_MONOTONIC or with
           FUTEX_CLOCK_REALTIME on the clock that says the date, and is turned
           into how long from now. And a waker wakes only the waiters whose
           bits it shares: here every waiter and waker shares all of them, and
           a waiter woken that Linux would have left asleep looks at its word
           and waits again, which a futex's user has to be ready for anyway.
           Refused as not implemented, a wait came straight back, and every
           thread of a Rust program waiting for a lock, a channel or a
           condition went round its loop as fast as it could. */
        struct lx_timespec left;
        if (op == 9 || op == 10) {
            if ((unsigned int)a6 == 0) {
                return -LX_EINVAL;
            }
            if (op == 10) {
                op = 1;
            } else {
                op = 0;
                if (a4) {
                    const struct lx_timespec *at = (const struct lx_timespec *)a4;
                    unsigned long deadline =
                        quark_nanos((unsigned long)at->tv_sec, (unsigned long)at->tv_nsec);
                    unsigned long now = (a2 & 256) ? __syscall1(SYS_CLOCK, QUARK_CLOCK_WALL) : quark_now();
                    unsigned long rest = deadline > now ? deadline - now : 0;
                    left.tv_sec = (long)(rest / 1000000000UL);
                    left.tv_nsec = (long)(rest % 1000000000UL);
                    a4 = (long)&left;
                }
            }
        }
        /* FUTEX_REQUEUE moves waiters from one word to wait on another, so
           that they wait for a lock rather than all wake to fight for it.
           Here it wakes them instead, as many as it would have woken and
           moved: each finds its word changed and goes to wait where it was
           to be moved to, which is what a futex's user does with a wake it
           did not expect. musl's condition variables hand each waiter a
           broadcast woke on to the next this way; refused, the first woke
           and the rest slept on for good. */
        if (op == 3) {
            if ((int)a3 < 0 || (int)a4 < 0) {
                return -LX_EINVAL;
            }
            unsigned long woken = __syscall2(SYS_FUTEX_WAKE, (unsigned long)a1,
                                             (unsigned long)(int)a3 + (unsigned long)(int)a4);
            return woken == QUARK_ERR ? -LX_EINVAL : (long)woken;
        }
        if (op == 0) {
            unsigned long r;
            if (a4) {
                const struct lx_timespec *ts = (const struct lx_timespec *)a4;
                unsigned long span =
                    quark_span(quark_nanos((unsigned long)ts->tv_sec, (unsigned long)ts->tv_nsec));
                r = __syscall3(SYS_FUTEX_WAIT_TIMEOUT, (unsigned long)a1, (unsigned long)a3, span);
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
            /* A signal ended the wait. A handler that did not ask for its
               calls to be made again is EINTR; anything else is a wake like
               any other, and the caller looks at the word again. */
            if (r == QUARK_INTERRUPTED) {
                return -LX_EINTR;
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
        /* The time this program, or this thread, has run: in it and in the
           kernel for it. */
        if (a1 == LX_CLOCK_PROCESS_CPUTIME || a1 == LX_CLOCK_THREAD_CPUTIME) {
            unsigned long u[4];
            usage_of(a1 == LX_CLOCK_PROCESS_CPUTIME ? 0 : 2, u);
            unsigned long ran = u[0] + u[1];
            ts->tv_sec = (long)(ran / 1000000000UL);
            ts->tv_nsec = (long)(ran % 1000000000UL);
            return 0;
        }
        /* Another thread's, or this program's by its process id. */
        if (a1 < 0) {
            unsigned long u[4];
            long r = cpu_clock(a1, u);
            if (r) {
                return r;
            }
            unsigned long ran = (a1 & 3) == 1 ? u[0] : u[0] + u[1];
            ts->tv_sec = (long)(ran / 1000000000UL);
            ts->tv_nsec = (long)(ran % 1000000000UL);
            return 0;
        }
        /* The kernel's clock, in nanoseconds. The real-time clocks are the
           date — the one read at boot, or set since — and every other clock
           counts from boot, which is what a monotonic clock is for. On a
           machine with no clock to say the date, the date is how long it
           has been on. */
        unsigned long now = wall_clock(a1) ? __syscall1(SYS_CLOCK, QUARK_CLOCK_WALL) : 0;
        if (!now) {
            now = quark_now();
        }
        ts->tv_sec = (long)(now / 1000000000UL);
        ts->tv_nsec = (long)(now % 1000000000UL);
        return 0;
    }
    case LX_clock_getres: {
        /* What the clock is kept in. How fine it really is depends on the
           machine, and on one with nothing better it moves ten milliseconds
           at a time; Linux says a nanosecond of a clock like that too. */
        struct lx_timespec *ts = (struct lx_timespec *)a2;
        unsigned long u[4];
        if (a1 < 0 && cpu_clock(a1, u)) {
            return -LX_EINVAL;
        }
        if (ts) {
            ts->tv_sec = 0;
            ts->tv_nsec = 1;
        }
        return 0;
    }
    case LX_gettimeofday: {
        long *tv = (long *)a1;
        if (tv) {
            unsigned long now = __syscall1(SYS_CLOCK, QUARK_CLOCK_WALL);
            if (!now) {
                now = quark_now();
            }
            tv[0] = (long)(now / 1000000000UL);
            tv[1] = (long)(now % 1000000000UL / 1000);
        }
        return 0;
    }
    case LX_time: {
        unsigned long now = __syscall1(SYS_CLOCK, QUARK_CLOCK_WALL);
        if (!now) {
            now = quark_now();
        }
        if (a1) {
            *(long *)a1 = (long)(now / 1000000000UL);
        }
        return (long)(now / 1000000000UL);
    }
    /* Setting the date. It is the machine's, and whoever may set it holds
       the capability to: a session of an account with the `clock` right,
       and what such a session starts. The kernel writes it through to the
       clock that keeps time while the machine is off. */
    case LX_clock_settime:
    case LX_settimeofday: {
        const long *t = (const long *)(n == LX_clock_settime ? a2 : a1);
        if (n == LX_clock_settime && !wall_clock(a1)) {
            return -LX_EINVAL;
        }
        if (!t) {
            /* settimeofday with only a time zone to say: there is none. */
            return n == LX_settimeofday ? 0 : -LX_EFAULT;
        }
        long sub = n == LX_clock_settime ? t[1] : t[1] * 1000;
        if (t[0] < 1 || t[0] >= 7258118400L || t[1] < 0 || sub >= 1000000000L) {
            return -LX_EINVAL;
        }
        unsigned long r = __syscall1(SYS_CLOCK_SET, (unsigned long)t[0] * 1000000000UL + (unsigned long)sub);
        return r == QUARK_ERR ? -LX_EPERM : 0;
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
     * The flags are read and not honoured: a timerfd that is not
     * close-on-exec is a distinction this system does not draw. The kernel
     * keeps its times to the nanosecond and fires it when it is due. Which
     * clock it was made on matters for one thing, a time given as what the
     * clock will read: the kernel's timers all count from boot, so a time
     * on the clock that says the date is turned into how far off it is
     * when it is set, and the date being set afterwards does not move it. */
    case LX_timerfd_create: {
        unsigned long fd = __syscall0(SYS_TIMER_CREATE);
        if (fd == QUARK_ERR) {
            return -LX_EMFILE;
        }
        if (fd < MAX_FDS) {
            if (wall_clock(a1)) {
                __atomic_fetch_or(&timer_wall, 1UL << fd, __ATOMIC_SEQ_CST);
            } else {
                __atomic_fetch_and(&timer_wall, ~(1UL << fd), __ATOMIC_SEQ_CST);
            }
        }
        return (long)fd;
    }
    case LX_timerfd_settime: {
        /* itimerspec: interval seconds and nanoseconds, then the same for the
           first expiration. Absolute deadlines (TFD_TIMER_ABSTIME) are turned
           into a delay here, since the kernel counts only forwards. */
        if (!a3) {
            return -LX_EINVAL;
        }
        const long *it = (const long *)a3;
        if (it[0] < 0 || it[2] < 0 || it[1] < 0 || it[1] >= 1000000000L || it[3] < 0
            || it[3] >= 1000000000L) {
            return -LX_EINVAL;
        }
        unsigned long interval = quark_nanos((unsigned long)it[0], (unsigned long)it[1]);
        unsigned long first = quark_nanos((unsigned long)it[2], (unsigned long)it[3]);
        if (first && (a2 & 1 /* TFD_TIMER_ABSTIME */)) {
            /* What the timer's clock will read. A time already past is a
               timer that fires at once. */
            int wall = a1 >= 0 && a1 < MAX_FDS
                       && (__atomic_load_n(&timer_wall, __ATOMIC_SEQ_CST) >> a1 & 1);
            unsigned long reads = wall ? __syscall1(SYS_CLOCK, QUARK_CLOCK_WALL) : quark_now();
            first = first > reads ? first - reads : 1;
        }
        if (a4) {
            long *old = (long *)a4;
            unsigned long was[2] = { 0, 0 };
            __syscall2(SYS_TIMER_GET, (unsigned long)a1, (unsigned long)was);
            old[0] = (long)(was[1] / 1000000000UL);
            old[1] = (long)(was[1] % 1000000000UL);
            old[2] = (long)(was[0] / 1000000000UL);
            old[3] = (long)(was[0] % 1000000000UL);
        }
        return __syscall3(SYS_TIMER_SET, (unsigned long)a1, first ? quark_span(first) : 0,
                          interval ? quark_span(interval) : 0) == QUARK_ERR
                   ? -LX_EINVAL
                   : 0;
    }
    case LX_timerfd_gettime: {
        unsigned long was[2] = { 0, 0 };
        if (__syscall2(SYS_TIMER_GET, (unsigned long)a1, (unsigned long)was) == QUARK_ERR) {
            return -LX_EINVAL;
        }
        if (a2) {
            long *out = (long *)a2;
            out[0] = (long)(was[1] / 1000000000UL);
            out[1] = (long)(was[1] % 1000000000UL);
            out[2] = (long)(was[0] / 1000000000UL);
            out[3] = (long)(was[0] % 1000000000UL);
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
    case LX_inotify_init:
        return __quark_inotify_init(0);
    case LX_inotify_init1:
        return __quark_inotify_init(a1);
    case LX_inotify_add_watch:
        return __quark_inotify_add_watch(a1, (const char *)a2, (unsigned long)a3);
    case LX_inotify_rm_watch:
        return __quark_inotify_rm_watch(a1, a2);
    case LX_signalfd:
        return __quark_signalfd(a1, (const unsigned long *)a2, (unsigned long)a3, 0);
    case LX_signalfd4:
        return __quark_signalfd(a1, (const unsigned long *)a2, (unsigned long)a3, a4);
    case LX_timer_create:
        return __quark_timer_create(a1, (const void *)a2, (int *)a3);
    case LX_timer_settime:
        return __quark_timer_settime(a1, a2, (const void *)a3, (void *)a4);
    case LX_timer_gettime:
        return __quark_timer_gettime(a1, (void *)a2);
    case LX_timer_getoverrun:
        return __quark_timer_getoverrun(a1);
    case LX_timer_delete:
        return __quark_timer_delete(a1);
    case LX_rt_sigqueueinfo:
        return __quark_sigqueue(a1, -1, a2, (const void *)a3);
    case LX_rt_tgsigqueueinfo:
        return __quark_sigqueue(a1, a2, a3, (const void *)a4);
    case LX_rt_sigreturn:
        /* Returning from a handler is returning from a function here. */
        return 0;
    /* The one interval timer there is: real time, and SIGALRM when it runs
       out. musl's `alarm` is a `setitimer`; the call of that name is here for
       a program that makes it itself. */
    case LX_alarm: {
        /* What was left of the one before, in whole seconds: rounded up,
           so that an alarm still to come is never said to be none. */
        unsigned long was[2];
        unsigned long r = do_alarm((unsigned long)(unsigned int)a1 * 1000000000UL, 0, 0, was);
        return r == QUARK_ERR ? 0 : (long)((was[0] + 999999999UL) / 1000000000UL);
    }
    case LX_setitimer:
        return do_itimer(a1, (const long *)a2, (long *)a3);
    case LX_getitimer:
        return do_itimer(a1, NULL, (long *)a2);
    /* Where this thread's robust list is: what the kernel walks if the thread
       dies holding one of its mutexes, which musl walks itself when a thread
       ends the ordinary way. musl asks get_robust_list first, to learn
       whether there is such a thing. */
    case LX_set_robust_list:
        if (a2 != 24) {
            return -LX_EINVAL;
        }
        return __syscall1(SYS_ROBUST_LIST, (unsigned long)a1) == QUARK_ERR ? -LX_EINVAL : 0;
    case LX_get_robust_list: {
        if (a1 != 0 && (unsigned long)a1 != __syscall0(SYS_GETPID)) {
            return -LX_EPERM;
        }
        unsigned long head = __syscall1(SYS_ROBUST_LIST, ~0UL);
        if (a2) {
            *(unsigned long *)a2 = head;
        }
        if (a3) {
            *(unsigned long *)a3 = 24;
        }
        return 0;
    }
    case LX_rseq:
        return -LX_ENOSYS;

    case LX_prctl:
        return prctl(a1, a2);

    /* Limits. Two are facts about this system — a program has sixty-four
       descriptors and a megabyte of stack — and the rest are not kept. */
    case LX_getrlimit:
        return do_getrlimit(a1, (unsigned long *)a2);
    case LX_setrlimit:
        return do_setrlimit(a1, (const unsigned long *)a2);
    case LX_prlimit64: {
        /* pid, resource, new, old: this process only. */
        if (a1 != 0 && a1 != __quark_getpid()) {
            return -LX_ESRCH;
        }
        if (a4) {
            long r = do_getrlimit(a2, (unsigned long *)a4);
            if (r) {
                return r;
            }
        }
        return a3 ? do_setrlimit(a2, (const unsigned long *)a3) : 0;
    }

    /* What was used, which the kernel counts: RUSAGE_SELF, RUSAGE_CHILDREN
       and RUSAGE_THREAD. */
    case LX_getrusage: {
        if (a1 != 0 && a1 != -1 && a1 != 1) {
            return -LX_EINVAL;
        }
        unsigned long u[4];
        usage_of(a1 == 0 ? 0 : a1 == -1 ? 1 : 2, u);
        if (a2) {
            fill_rusage((void *)a2, u);
        }
        return 0;
    }
    case LX_times: {
        if (a1) {
            /* In ticks, a hundred a second. */
            unsigned long self[4], children[4];
            usage_of(0, self);
            usage_of(1, children);
            long *t = (long *)a1;
            t[0] = (long)(self[0] / 10000000UL);
            t[1] = (long)(self[1] / 10000000UL);
            t[2] = (long)(children[0] / 10000000UL);
            t[3] = (long)(children[1] / 10000000UL);
        }
        /* Ticks since boot, at the hundred a second Linux counts in too. */
        return (long)__syscall0(SYS_TICKS);
    }
    case LX_sysinfo:
        return do_sysinfo((unsigned long *)a1);

    case LX_select: {
        const long *tv = (const long *)a5;
        return do_select(a1, (unsigned long *)a2, (unsigned long *)a3, (unsigned long *)a4,
                         tv ? wait_ns(tv[0], tv[1] * 1000) : -1, NULL);
    }
    case LX_pselect6: {
        const long *ts = (const long *)a5;
        long ns = ts ? wait_ns(ts[0], ts[1]) : -1;
        /* The sixth argument is a pointer to a mask and its size. */
        const unsigned long *const *sixth = (const unsigned long *const *)a6;
        return do_select(a1, (unsigned long *)a2, (unsigned long *)a3, (unsigned long *)a4, ns,
                         sixth ? sixth[0] : NULL);
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

    /* Off, and on again. The kernel does either for a program that holds
       the right to — root's shell does, and what it starts — and what has
       been written is this program's to have had recorded first (`sync`),
       as it is on Linux. Linux also takes a command here that says what
       Ctrl-Alt-Del is to do; there is nothing here for that to change. */
    case LX_reboot: {
        if ((unsigned int)a1 != 0xfee1dead) {
            return -LX_EINVAL;
        }
        switch ((unsigned int)a3) {
        case 0x01234567: /* restart */
            __syscall1(SYS_POWER, 1);
            return -LX_EPERM;
        case 0x4321fedc: /* power off */
        case 0xcdef0123: /* halt */
            __syscall1(SYS_POWER, 0);
            return -LX_EPERM;
        case 0x89abcdef: /* Ctrl-Alt-Del restarts */
        case 0x00000000: /* Ctrl-Alt-Del signals init */
            return 0;
        default:
            return -LX_EINVAL;
        }
    }

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
        return __quark_socket(a1, a2, a3);
    case LX_bind:
        return __quark_bind(a1, (const void *)a2, (unsigned long)a3);
    case LX_listen:
        return __quark_listen(a1, a2);
    case LX_accept:
        return __quark_accept(a1, (void *)a2, (unsigned int *)a3, 0);
    case LX_accept4:
        return __quark_accept(a1, (void *)a2, (unsigned int *)a3, a4);
    case LX_connect:
        return __quark_connect(a1, (const void *)a2, (unsigned long)a3);
    case LX_getsockname:
        return __quark_sockname(a1, (void *)a2, (unsigned int *)a3, 0);
    case LX_getpeername:
        return __quark_sockname(a1, (void *)a2, (unsigned int *)a3, 1);
    case LX_getsockopt:
        return __quark_getsockopt(a1, a2, a3, (void *)a4, (unsigned int *)a5);
    case LX_setsockopt:
        return __quark_setsockopt(a1, a2, a3, (const void *)a4, (unsigned long)a5);
    case LX_sendto:
        return __quark_sendto(a1, (const void *)a2, (unsigned long)a3, a4, (const void *)a5, (unsigned long)a6);
    case LX_recvfrom:
        return __quark_recvfrom(a1, (void *)a2, (unsigned long)a3, a4, (void *)a5, (unsigned int *)a6);
    case LX_sendmsg:
        return __quark_sendmsg(a1, (const void *)a2, a3);
    case LX_recvmsg:
        return __quark_recvmsg(a1, (void *)a2, a3);
    case LX_shutdown:
        return __quark_shutdown(a1, a2);
    case LX_poll:
        return __quark_poll((void *)a1, a2, wait_ms((int)a3), NULL);
    case LX_ppoll: {
        /* A timespec rather than milliseconds, and a mask to wait under. */
        const long *ts = (const long *)a3;
        long ns = ts ? wait_ns(ts[0], ts[1]) : -1;
        return __quark_poll((void *)a1, a2, ns, (const unsigned long *)a4);
    }
    case LX_epoll_pwait:
        return __quark_epoll_wait(a1, (void *)a2, a3, wait_ms((int)a4), (const unsigned long *)a5);
    case LX_epoll_create1:
        return __quark_epoll_create(a1);
    case LX_epoll_ctl:
        return __quark_epoll_ctl(a1, a2, a3, (void *)a4);
    case LX_epoll_wait:
        return __quark_epoll_wait(a1, (void *)a2, a3, wait_ms((int)a4), NULL);

    /* statx carries more than the VFS knows, and musl falls back to the plain
       stat calls when it is refused. */
    case LX_statx:
        return -LX_ENOSYS;

    default:
        trace("nosys", n);
        return -LX_ENOSYS;
    }
}
