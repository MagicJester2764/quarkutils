/* Raw Quark system calls.
 *
 * The numbers come from docs/abi.md, which is the contract: a libc is a
 * consumer of that ABI exactly as the Rust runtime is, and neither is
 * privileged over the other.
 */
#ifndef _QUARK_SYSCALL_H
#define _QUARK_SYSCALL_H

#include <stddef.h>

/* 0x00  process */
#define SYS_EXIT            0
#define SYS_EXIT_CODE       1
#define SYS_EXIT_PROGRAM    8
#define SYS_UMASK           9
#define SYS_WAIT_FOR        10
#define SYS_SIG_ACTION      11
#define SYS_SIG_RAISE       12
#define SYS_SIG_TAKE        13
/* The process id of the program a task belongs to (0: the caller's). A task
   id is reused at once; this never is. SYS_WAIT_FOR and SYS_SIG_RAISE can
   each be asked by it. */
#define SYS_PID             14
#define QUARK_WAIT_NOW      1UL
#define QUARK_WAIT_BY_PID   2UL
/* What SYS_SIG_RAISE's first argument names: a task of the program, or the
   program's process id. Said, every time: it is the call's third argument. */
#define QUARK_RAISE_BY_TASK 0UL
#define QUARK_RAISE_BY_PID  1UL
/* For the task named and no other: run there, and held back there, it
   waits there. */
#define QUARK_RAISE_THREAD  4UL
/* Jobs. A wait can ask to hear of a child that has stopped, or been started
   again, and can name a process group of children (0 for the caller's own);
   an answer about a stop or a start has QUARK_WAIT_REPORT beside the child's
   name, and the signal that stopped it above — 0 for a start. A signal can be
   raised for a group. */
#define QUARK_WAIT_STOPPED   4UL
#define QUARK_WAIT_CONTINUED 8UL
#define QUARK_WAIT_GROUP     16UL
#define QUARK_WAIT_REPORT    (1UL << 31)
#define QUARK_RAISE_GROUP    2UL
/* What a call about groups, sessions or a terminal's foreground answers when
   the rules say no, as distinct from there being nothing of the kind. */
#define QUARK_NOT_ALLOWED    ((unsigned long)-2)
/* Have signal 14 raised for the program so many ticks from now, and then
   every so many: the answer is what was left of the alarm this replaces, and
   above it what that one repeated at. */
#define SYS_SIG_ALARM       15
#define QUARK_ALARM_ASK     1UL
#define SYS_YIELD           2
#define SYS_GETPID          3
#define SYS_RECV_TIMEOUT    21
#define SYS_WAIT            4
#define SYS_TASK_KILL       5
#define SYS_TASK_INFO       7
/* Who the caller is: its user id above its group id. */
#define SYS_GET_UID         98
#define SYS_SET_UID         99
#define SYS_SET_GID         100

/* 0x10  IPC */
#define SYS_SEND            16
#define SYS_RECV            17
#define SYS_CALL            18
#define SYS_REPLY           19
#define SYS_CALL_LEND       23
#define SYS_LENT_READ       25
#define SYS_LENT_WRITE      26

/* SYS_CALL_LEND's last argument is the length with these above it: what the
   task called may do with the buffer until it replies. */
#define QUARK_LEND_READ     (1UL << 62)
#define QUARK_LEND_WRITE    (1UL << 63)

/* 0x20  memory */
#define SYS_MMAP            32
#define SYS_MUNMAP          33
#define SYS_PHYS_ALLOC      34
#define SYS_MAP_PHYS        38
#define SYS_MMAP_FD         42
#define SYS_ADDRSPACE_SELF  41
#define SYS_ADDRSPACE_CREATE 36
#define SYS_ADDRSPACE_GIVE  43
#define SYS_ADDRSPACE_DESTROY 44

/* 0x40  file descriptors */
#define SYS_FD_READ         64
#define SYS_FD_WRITE        65
#define SYS_FD_READ_NB      66
#define SYS_FD_WRITE_NB     79
#define SYS_FD_DUP          68
#define SYS_PIPE_CREATE     69
#define SYS_PIPE_FD_SET     70
#define SYS_FD_CLOSE        71
#define SYS_SOCKETPAIR      72
#define SYS_FD_SEND         73
#define SYS_FD_RECV         74
#define SYS_POLLSET_CREATE  75
#define SYS_POLLSET_CTL     76
#define SYS_POLLSET_WAIT    77
#define SYS_POLL            78

/* 0x30  shared memory */
#define SYS_MEMFD_CREATE    53
#define SYS_MEMFD_TRUNCATE  54

/* 0x60  task */
#define SYS_TASK_CREATE     96
#define SYS_SET_FS_BASE     102
#define SYS_TASK_START_ARG  103
#define SYS_SET_CLEAR_TID   106
#define SYS_TASK_WATCH      104
#define SYS_TASK_SPACE      107
#define SYS_SPACE_WATCH     108
#define SYS_FORK            110
#define SYS_EXEC_SPACE      111

/* 0xD0  terminals */
#define SYS_PTY_CREATE      208
#define SYS_PTY_CTL         209
#define SYS_PTY_OPEN        210
/* Process groups and sessions: arg0 says what is asked, arg1 names a process
   (0 for the caller's own), arg2 a group for the one that sets it. */
#define SYS_PGROUP          211
#define QUARK_PGROUP_GET    0UL
#define QUARK_PGROUP_SET    1UL
#define QUARK_SESSION_GET   2UL
#define QUARK_SESSION_NEW   3UL
/* The groups a task is in besides its own: arg0 = 0 to read and 1 to set,
   arg1 a task (0 for the caller), arg2 where the ids are, arg3 how many.
   And saying who a task is, which is a server's to do. */
#define SYS_GROUPS          212
#define QUARK_GROUPS_GET    0UL
#define QUARK_GROUPS_SET    1UL
#define QUARK_MAX_GROUPS    16
#define SYS_IDENTIFY        213
#define SYS_PROGRAM_NAME    214

/* 0x90  time */
#define SYS_TIMER_CREATE    146
#define SYS_TIMER_SET       147
#define SYS_TIMER_GET       148

/* 0x70  hardware */
#define SYS_GETRANDOM       116
#define SYS_CPUS            117

/* 0x80  futex */
#define SYS_FUTEX_WAIT      128
#define SYS_FUTEX_WAKE      129
#define SYS_FUTEX_WAIT_TIMEOUT 130
#define SYS_EVENT_CREATE    131
/* A signal that carries a value: a real-time one queues. */
#define SYS_SIG_QUEUE       136
/* A descriptor read for signals: signalfd. */
#define SYS_SIGNAL_FD       137
/* Where the caller's program makes its system calls from: one made from
   anywhere else raises SIGSYS. Linux's syscall user dispatch. */
#define SYS_SYSCALL_TRAP    138

/* 0x90  time
 *
 * A span of time handed to the kernel — a timeout, a timer, an alarm — is a
 * count of ticks, hundredths of a second, or with QUARK_SPAN_NS set a count
 * of nanoseconds (quark_span). */
#define SYS_TICKS           144
#define SYS_BOOT_TIME       145
/* What time it is, in nanoseconds: since boot, which only goes forward and
   is what a wait is measured by; or with QUARK_CLOCK_WALL since 1970, which
   is 0 on a machine with no clock to say. */
#define SYS_CLOCK           149
#define QUARK_CLOCK_WALL    1UL
/* Say what time it is: nanoseconds since 1970. For a holder of the right. */
#define SYS_CLOCK_SET       150
/* A program's timer that raises a signal: by what the first argument
   says, make one, set it, say how it stands, end it. */
#define SYS_PTIMER          151
#define QUARK_SPAN_NS       (1UL << 63)

/* 0xC0  memory, continued */
#define SYS_MAP_ANON        192
#define SYS_MEM_INFO        193
#define QUARK_MAP_POPULATE  1UL
#define QUARK_MAP_ACCOUNT   2UL
#define SYS_OBJECT_MAP      195
#define SYS_OBJECT_SYNC     197
#define SYS_PAGE_OUT        198
#define QUARK_OBJECT_WRITE  1UL
#define QUARK_OBJECT_SHARED 2UL
#define QUARK_OBJECT_EXEC   4UL
#define SYS_CAP_DELETE      84
/* Make a capability in the caller's own slot arg0, of kind arg1 with
   parameters arg2 and arg3: within one it holds of that kind. */
#define SYS_CAP_MINT        80
/* What one slot of a task's capabilities holds: arg0 a task, arg1 the slot,
   arg2 four words for its kind, its two parameters and whether it is live.
   A task may read its own. It answers how many slots the space has room
   for: every slot past that is empty. */
#define SYS_CAP_READ        92
/* A space's room before anything has been written in it, and the least a
   walk of its slots looks at. */
#define QUARK_CSPACE_FIRST  256UL
/* The kind that lets its holder say who a task is. */
#define QUARK_CAP_TYPE_SET_UID 6UL

/* 0xA0  kernel console */
#define SYS_WRITE           160

/* 0xE0  descriptors, continued */
#define SYS_FD_SERVED       225
#define SYS_FD_FLAGS        228
/* SYS_FD_FLAGS' second argument: read the flags, or set them. */
#define QUARK_FD_GETFLAGS   0UL
#define QUARK_FD_SETFLAGS   1UL
/* The working directory's descriptor, one past the ordinary numbers. */
#define QUARK_FD_CWD        0xFFFFFFFFFFFFFF9CUL
#define QUARK_FD_CLOEXEC    1UL
/* What a descriptor names, and above it a bit for "its other end has gone". */
#define SYS_FD_KIND         230
#define QUARK_FD_KIND(k)        ((k) & 0xFFUL)
#define QUARK_FD_GONE           0x100UL
#define QUARK_FD_KIND_ENDPOINT   1UL
#define QUARK_FD_KIND_PIPE_READ  2UL
#define QUARK_FD_KIND_PIPE_WRITE 3UL
#define QUARK_FD_KIND_STREAM     4UL
#define QUARK_FD_KIND_PTY_MASTER 5UL
#define QUARK_FD_KIND_PTY_SLAVE  6UL
#define QUARK_FD_KIND_TIMER      7UL
#define QUARK_FD_KIND_EVENT      8UL
#define QUARK_FD_KIND_POLLSET    9UL
#define QUARK_FD_KIND_MEMORY     10UL
#define QUARK_FD_KIND_SOCKET     11UL
#define QUARK_FD_KIND_SERVED     12UL
#define QUARK_FD_KIND_SIGNALS    13UL
#define QUARK_FD_KIND_LOCAL      14UL
/* A named pipe. A server gives a client an end of the pipe a key names, and
   whoever was given one waits here for the other end to be opened, with the
   number that came with it. */
#define SYS_FD_SERVE_PIPE   231
#define SYS_PIPE_PEER       232
/* A server says what an object of its own is ready for, to a poll: one it
   made saying it would, which is not a file. */
#define SYS_FD_READY        233
/* A connected pair whose writes are messages, each read whole:
   socketpair of SOCK_SEQPACKET. */
#define SYS_PACKET_PAIR     234
#define SYS_FD_LIMIT        235
#define SYS_TASK_NEXT       236
/* Wake some of a futex word's waiters and move the rest to another word:
   arg0 the first, arg1 the second, arg2 (how many to wake << 32) | how many
   to move, arg3 what the first must hold and arg4 bit 0 to compare it. It
   answers how many, or QUARK_FUTEX_CHANGED. */
#define SYS_FUTEX_REQUEUE   237
#define QUARK_FUTEX_CHANGED 0xFFFFFFFEUL
/* How one task is scheduled: arg0 the op — 0 how nice (20 + it), 1 make it
   as nice as arg2, 2 put it in class arg2 (0 ordinary, 1 FIFO, 2 round-robin)
   at real-time priority arg3, 3 its class, (class << 8) | priority — and
   arg1 the task, 0 for the caller. Entering a real-time class takes the
   right to (QUARK_CAP_REALTIME). */
#define SYS_SCHED           238
#define QUARK_SCHED_NICE      0
#define QUARK_SCHED_SET_NICE  1
#define QUARK_SCHED_SET_CLASS 2
#define QUARK_SCHED_CLASS     3
/* A lock that lends its holder the place of whoever waits for it, Linux's
   FUTEX_LOCK_PI, FUTEX_TRYLOCK_PI and FUTEX_UNLOCK_PI: arg0 the op, arg1 the
   word — nought, or its holder's task id with bit 31 while somebody waits in
   the kernel and bit 30 when its holder died — and arg2 for a lock how long
   to wait at most (a span; 0 for no end). It answers 0, 2 for a timeout,
   QUARK_INTERRUPTED, QUARK_NOT_ALLOWED for an unlock of a word the caller
   does not hold, or one of these. */
#define SYS_FUTEX_PI        239
#define QUARK_PI_LOCK         0
#define QUARK_PI_TRY          1
#define QUARK_PI_UNLOCK       2
#define QUARK_PI_BUSY         1UL
#define QUARK_PI_DEADLOCK     3UL
#define QUARK_PI_NO_OWNER     4UL
/* What a task is called, Linux's comm: arg0 = the task, arg1 = 0 set or 1
   read, arg2 = the name or where it goes, arg3 = its length or the room. */
#define SYS_TASK_NAME       215
/* What each processor is and does (from 4.6): arg0 = op. 0, which are
   online, a bit each of 256 at arg1 (arg2 bytes, 32 at least), answering how
   many; 1, how processor arg1 (or all, for u64 all ones) has spent its
   time, eight words at arg2 — ns in programs, in the kernel, idle, taking
   interrupts; interrupts; switches; tasks made since the machine started;
   tasks running or ready; 2, where processor arg1 sits, four words at arg2 —
   APIC id, package, core, thread; 3, the processor task arg1 (0, the
   caller) last ran on. */
#define SYS_CPU_INFO        218
#define QUARK_CPU_ONLINE      0UL
#define QUARK_CPU_TIMES       1UL
#define QUARK_CPU_PLACE       2UL
#define QUARK_CPU_LAST        3UL
/* Which processors a task may run on (from 4.7): arg0 = 0 read or 1 set,
   arg1 = the task (0 the caller), arg2 = 256 bits, four words. */
#define SYS_AFFINITY        219
/* A processor taken offline (arg0 = 0) or brought back (1), arg1 = which
   (from 4.8): not the first, and only with QUARK_CAP_PROCESSORS. */
#define SYS_CPU_ONLINE      220
/* Where the caller's robust list is (set_robust_list), or u64 all ones to
   ask. */
#define SYS_ROBUST_LIST     132

/* 0xF0  introspection */
/* Turn the machine off (0) or start it again (1): for a holder of the right
   to. It returns only if the machine is still on. In the hardware block. */
#define SYS_POWER           119
#define SYS_SIG_MASK        120
#define SYS_SIG_RETURN      121
#define SYS_SIG_STACK       122
#define SYS_SIG_WAIT        123
#define SYS_USAGE           124
#define SYS_NICE            125
#define SYS_CPU_LIMIT       126
/* 0xB0  sockets: a local socket by a name — one that is nothing yet; a
   server names one a client holds, or connects it to what listens at a
   name; listen; accept; who is at the other end; whether to be told who
   sent what. */
#define SYS_SOCKET          178
#define SYS_SOCKET_BIND     179
#define SYS_SOCKET_LISTEN   180
#define SYS_SOCKET_CONNECT  181
#define SYS_SOCKET_ACCEPT   182
#define SYS_SOCKET_PEER     183
#define SYS_SOCKET_OPTION   184
#define SYS_ABI_VERSION     240

/* What the kernel returns for "no". Not an errno: each call says what it
   means, and the wrappers here translate. */
#define QUARK_ERR ((unsigned long)-1)

/* SYS_FD_RECV's `at` when any free descriptor will do. The kernel picks the
   number and returns it, which is what a caller translating `recvmsg` needs:
   it cannot see the kernel's half of the table to pick one itself. */
#define QUARK_ANY_FD ((unsigned long)-2)

/* SYS_FD_SEND and SYS_FD_RECV flags. QUARK_DONTWAIT is MSG_DONTWAIT: return
   QUARK_WOULD_BLOCK rather than parking. */
#define QUARK_DONTWAIT      1UL
/* And QUARK_FD_MANY: the fourth argument is an array of descriptors, u32s,
   as many as bits 8 to 15 of the flags say — to send, or room for those
   received, and a receive answers (how many << 32) | bytes. */
#define QUARK_FD_MANY       2UL
#define QUARK_WOULD_BLOCK   ((unsigned long)0xFFFFFFFEUL)

/* What a wait answers when a signal ended it; a sleep (SYS_RECV_TIMEOUT on
   the caller's own id) answers QUARK_SLEEP_INTERRUPTED, where 1 is the time
   running out. To a program that has said it wants Unix's answers
   (QUARK_SIG_UNIX, below) it says what came of it: QUARK_INTERRUPTED, a
   handler ran that did not ask for SA_RESTART; QUARK_RESTART, one ran that
   did; QUARK_AGAIN, nothing ran here, and the call is made again. */
#define QUARK_INTERRUPTED       ((unsigned long)0xFFFFFFFDUL)
#define QUARK_RESTART           ((unsigned long)0xFFFFFFFCUL)
#define QUARK_AGAIN             ((unsigned long)0xFFFFFFFBUL)
#define QUARK_SLEEP_INTERRUPTED 2UL
/* SYS_SIG_ACTION's second argument: what the signal does, nothing, the
   program is told and runs a handler itself, or the kernel runs it. For
   signal 0 with QUARK_SIG_RUN, the fifth argument is where handlers are
   entered and the fourth has QUARK_SIG_UNIX for Unix's answers. */
#define QUARK_SIG_DEFAULT   0UL
#define QUARK_SIG_IGNORE    1UL
#define QUARK_SIG_HANDLE    2UL
#define QUARK_SIG_RUN       3UL
#define QUARK_SIG_UNIX      1UL
#define QUARK_SIG_ASK       ((unsigned long)-1)
/* SYS_SIG_MASK's first argument besides SIG_BLOCK (0), SIG_UNBLOCK (1) and
   SIG_SETMASK (2): wait under the mask given; say what is waiting and held
   back. */
#define QUARK_SIG_MASK_WAIT    3UL
#define QUARK_SIG_MASK_PENDING 4UL
/* SYS_POLL's fifth argument, for a mask to wait under in its fourth; and
   SYS_POLLSET_WAIT's fifth, a mask with this bit — signal 9's, which is
   never held back — set to say it is one. */
#define QUARK_POLL_UNDER       1UL
#define QUARK_POLLSET_UNDER    (1UL << 8)
/* SYS_POLLSET_CTL: op | QUARK_POLLSET_WHY says why it refused, as one of the
   QUARK_POLLSET_* reasons; and a watch's events besides 1 readable and 2
   writable — the other end gone, said beside a hangup; an edge; a
   one-shot. */
#define QUARK_POLLSET_WHY      (1UL << 8)
#define QUARK_POLLSET_NOT_ONE  1UL
#define QUARK_POLLSET_EXISTS   2UL
#define QUARK_POLLSET_ABSENT   3UL
#define QUARK_POLLSET_CANNOT   4UL
#define QUARK_POLLSET_LOOP     5UL
#define QUARK_POLLSET_FULL     6UL
#define QUARK_POLL_PEER_GONE   0x10UL
#define QUARK_POLL_EDGE        (1UL << 16)
#define QUARK_POLL_ONCE        (1UL << 17)

/* The system call wrappers.
 *
 * The kernel's entry path does not preserve the argument registers: it
 * shuffles them into the C ABI its dispatcher expects and leaves rdi, rsi,
 * rdx, r8, r9 and r10 as whatever that left behind. So every one of them is
 * declared written, not just the rcx and r11 the `syscall` instruction itself
 * takes. Getting this wrong does not fail near the call — it fails wherever
 * the compiler had chosen to keep a live value, which was `&free_list` inside
 * malloc the first time.
 *
 * Argument registers are `+` rather than inputs for the same reason: an input
 * cannot also be a clobber, and these are both.
 */
#define __SYSCALL_CLOBBERS "rcx", "r11", "memory"

/* arg3 travels in r10, not rcx: the syscall instruction overwrites rcx with
   the return address before the kernel ever sees it. arg4 travels in r8. */
static inline unsigned long __syscall5(unsigned long n, unsigned long a, unsigned long b,
                                       unsigned long c, unsigned long d, unsigned long e) {
    unsigned long r;
    register unsigned long r10 __asm__("r10") = d;
    register unsigned long r8 __asm__("r8") = e;
    __asm__ volatile("syscall"
                     : "=a"(r), "+D"(a), "+S"(b), "+d"(c), "+r"(r10), "+r"(r8)
                     : "a"(n)
                     : "r9", __SYSCALL_CLOBBERS);
    return r;
}

/* A call with fewer arguments is the call with five, and zeroes.
 *
 * Not a convenience. The kernel reads the registers a call is documented to
 * take, and a call that is given another argument in a later version reads
 * it from every caller there is — including the ones written before, which
 * passed two and left in the third register whatever they had last
 * computed. `SYS_SIG_RAISE` gained a third argument saying what its first
 * one names, and the two-argument calls here went on being made: for a long
 * time what was left in the register happened to say "a task", and then a
 * change nearby moved a value, and `raise(SIGSTOP)` was a signal for a
 * process group that did not exist. An argument not given is given as
 * nothing.
 */
static inline unsigned long __syscall4(unsigned long n, unsigned long a, unsigned long b,
                                       unsigned long c, unsigned long d) {
    return __syscall5(n, a, b, c, d, 0);
}
static inline unsigned long __syscall3(unsigned long n, unsigned long a, unsigned long b,
                                       unsigned long c) {
    return __syscall5(n, a, b, c, 0, 0);
}
static inline unsigned long __syscall2(unsigned long n, unsigned long a, unsigned long b) {
    return __syscall5(n, a, b, 0, 0, 0);
}
static inline unsigned long __syscall1(unsigned long n, unsigned long a) {
    return __syscall5(n, a, 0, 0, 0, 0);
}
static inline unsigned long __syscall0(unsigned long n) {
    return __syscall5(n, 0, 0, 0, 0, 0);
}

/* A span of `ns` nanoseconds, for a call that takes one. Too long to say —
   292 years — is as long as can be said. */
static inline unsigned long quark_span(unsigned long ns) {
    return ns >= QUARK_SPAN_NS ? ~0UL : (QUARK_SPAN_NS | ns);
}

/* Seconds and nanoseconds as nanoseconds; more than can be counted is as
   many as can be. */
static inline unsigned long quark_nanos(unsigned long sec, unsigned long nsec) {
    return sec >= 9223372036UL ? QUARK_SPAN_NS - 1 : sec * 1000000000UL + nsec;
}

/* Nanoseconds since boot. */
static inline unsigned long quark_now(void) {
    return __syscall1(SYS_CLOCK, 0);
}

/* A fixed-size IPC message: the only shape the kernel carries. */
struct quark_msg {
    unsigned long sender;
    unsigned long tag;
    unsigned long data[6];
};

/* Send and wait for the reply. Returns 0, or -1 if the call could not be made. */
int quark_call(size_t dest, const struct quark_msg *msg, struct quark_msg *reply);

/* As quark_call, lending `dest` the `len` bytes at `buf` until it replies:
   QUARK_LEND_READ lets it read them, QUARK_LEND_WRITE lets it fill them. */
int quark_call_lend(size_t dest, const struct quark_msg *msg, struct quark_msg *reply,
                    void *buf, unsigned long len, unsigned long access);

/* Find a service by name. Returns its task ID, or 0 if there is none. */
size_t quark_lookup(const char *name);

/* The task ID of the VFS, looked up once and remembered. 0 if there is none. */
size_t quark_vfs(void);

#endif
