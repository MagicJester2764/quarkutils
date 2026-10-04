/* Signals, for a libc that thinks it is talking to Linux.
 *
 * The kernel runs the handlers, as Linux's does. Told that a program has a
 * handler for a signal, it turns a thread of the program aside when the
 * signal is raised — on its way out of whatever call, interrupt or fault it
 * is in, so within a tick whatever it is doing — and enters the program at
 * one place, `__quark_sig_entry` below, with a record on the thread's stack
 * of where it was. That place keeps the floating-point registers, makes
 * the siginfo_t and ucontext_t a handler expects out of the record, calls
 * the handler, puts back what the handler changed of the context, and gives
 * the record back (SYS_SIG_RETURN). The thread then goes on from where the
 * record says.
 *
 * The mask is the kernel's too, a thread's own, and so is everything that
 * depends on it: a signal held back by every thread waits, whatever it
 * would do; sigsuspend, ppoll, pselect and epoll_pwait wait under another
 * mask in one step; sigtimedwait takes a signal without running anything.
 * A signal for one thread (tkill, raise) is that thread's.
 *
 * A call a signal cuts short is answered as Unix would have it, because
 * this program says so as it starts (`__quark_sig_start`): QUARK_INTERRUPTED
 * if the handler that ran did not ask for SA_RESTART, QUARK_RESTART if it
 * did, QUARK_AGAIN if nothing ran here — another thread took the signal, or
 * it was a stop and a continue. Each call that waits does what Linux does
 * with that (`quark_restartable`, `quark_cut_short`): a read, a write and a
 * wait for a child are made again or fail with EINTR as the handler asked;
 * a sleep, a poll and a sigsuspend are never made again; and none of them
 * is told of an AGAIN.
 *
 * Two signals the kernel raises of its own accord, and both arrive here like
 * any other: SIGALRM, when the alarm `setitimer` set is due, and SIGCHLD,
 * when a child of this program ends.
 *
 * And a third, in a program built for Linux (`__quark_linux_start`): SIGSYS,
 * for a system call the program's own code made rather than asking this
 * library to. The kernel makes no such call — its numbers are Linux's — and
 * hands it here instead, as a fault, with every register as it was; this
 * answers it as it answers its own calls, and the program goes on after it
 * as if the kernel had.
 */

#include <quark/syscall.h>

#include "abi.h"

#define NSIG 64
#define BIT(sig) (1UL << ((sig) - 1))

#define LX_SIGILL  4
#define LX_SIGBUS  7
#define LX_SIGFPE  8
#define LX_SIGKILL 9
#define LX_SIGSEGV 11
#define LX_SIGPIPE 13
#define LX_SIGSTOP 19
#define LX_SIGSYS  31
/* A SIGSYS for a call the kernel turned aside (Linux's SYS_USER_DISPATCH). */
#define LX_SYS_USER_DISPATCH 2
/* musl's own, for pthread_cancel. */
#define LX_SIGCANCEL 33

#define LX_SIG_DFL 0UL
#define LX_SIG_IGN 1UL

#define LX_SA_SIGINFO   4UL
#define LX_SA_ONSTACK   0x08000000UL
#define LX_SA_RESTART   0x10000000UL
#define LX_SA_NODEFER   0x40000000UL
#define LX_SA_RESETHAND 0x80000000UL

#define LX_SS_ONSTACK 1
#define LX_SS_DISABLE 2
#define LX_MINSIGSTKSZ 2048

/* What the kernel is told about running a handler (SYS_SIG_ACTION's flags):
   its own signal is not held back while it runs; it is run once; on the
   stack named for handlers; a call it cuts short is made again. And above
   the kernel's byte, a bit of this program's own that the kernel hands back
   in the frame: the handler takes three arguments. */
#define Q_NODEFER   1UL
#define Q_RESETHAND 2UL
#define Q_ONSTACK   4UL
#define Q_RESTARTS  8UL
#define Q_SIGINFO   0x100UL

/* The two a program may not refuse. */
#define UNBLOCKABLE (BIT(LX_SIGKILL) | BIT(LX_SIGSTOP))

/* What the kernel leaves on the stack when it runs a handler. */
struct quark_sigframe {
    unsigned long signo;
    /* 0: a program raised it, and `value` is its process id with the top
       bit set; 1: the kernel did; 2: the thread faulted, at `value`. */
    unsigned long code;
    unsigned long value;
    /* The thread's mask before, which is its mask again afterwards. */
    unsigned long mask;
    /* Bit 0: on the stack named for handlers. Above: the flags the handler
       was given with, this program's own bits among them. */
    unsigned long flags;
    /* The handler, as it was given. */
    unsigned long cookie;
    unsigned long regs[18];
    /* What came with it, which `code` and `value` say part of: Linux's
       si_code, who raised it (a process id, and its user in the high half;
       for SIGCHLD the child's), and what it carried — a queued value, a
       child's status, a fault's address. */
    long info_code;
    unsigned long info_who;
    unsigned long info_value;
};

/* musl's siginfo_t and ucontext_t on x86-64, laid out as a handler compiled
   against it reads them. */
struct lx_siginfo {
    int si_signo;
    int si_errno;
    int si_code;
    int pad;
    union {
        /* A signal a program or the kernel raised: who, and what it carried
           — si_value, or for SIGCHLD si_status. */
        struct {
            int pid;
            unsigned int uid;
            unsigned long value;
        } rt;
        void *addr;
        char fill[112];
    } u;
};
struct lx_ucontext {
    unsigned long uc_flags;
    struct lx_ucontext *uc_link;
    void *ss_sp;
    int ss_flags;
    unsigned long ss_size;
    unsigned long gregs[23];
    void *fpregs;
    unsigned long reserved[8];
    unsigned long sigmask[16];
    unsigned long fpregs_mem[64];
};
/* Where each register is in gregs. */
enum {
    G_R8, G_R9, G_R10, G_R11, G_R12, G_R13, G_R14, G_R15, G_RDI, G_RSI, G_RBP,
    G_RBX, G_RDX, G_RAX, G_RCX, G_RSP, G_RIP, G_EFL, G_CSGSFS, G_ERR, G_TRAPNO,
    G_OLDMASK, G_CR2
};
/* Each of the frame's registers — RAX RBX RCX RDX RSI RDI RBP R8 to R15 RIP
   RFLAGS RSP — at its place in gregs. */
static const unsigned char greg_of[18] = {
    G_RAX, G_RBX, G_RCX, G_RDX, G_RSI, G_RDI, G_RBP, G_R8, G_R9, G_R10, G_R11,
    G_R12, G_R13, G_R14, G_R15, G_RIP, G_EFL, G_RSP
};

/* What a program has said about each signal, for sigaction to say back. The
   kernel runs the handlers from what it was told, so this is read only by
   sigaction — which a handler may call. A writer holds `writing`, with every
   signal held back in its own thread so that no handler of its own finds it
   held; a reader goes round again while `seq` is odd or moves. */
static struct lx_ksigaction actions[NSIG + 1];
static int writing;
static volatile unsigned long seq;
/* Which of those it has said since it started. One it has not is as it was
   started: nothing said — or ignored, if whatever exec'd it was ignoring the
   signal, which the kernel kept and this program's memory did not. */
static unsigned long known;

/* How the floating-point state is kept around a handler: by XSAVE, in this
   many bytes, or by FXSAVE in 512. Set as the program starts. */
unsigned long __quark_fp_size = 512;
unsigned char __quark_fp_xsave;
unsigned int __quark_mxcsr_default = 0x1F80;

static unsigned long self(void) {
    return __syscall0(SYS_GETPID);
}

/* Where the kernel enters the program to run a handler: RDI is the frame,
   and the stack is as a function finds it. The floating-point registers are
   kept below it — the header XRSTOR reads cleared first, since XSAVE does
   not write all of it — and the handler is given a clean set of its own.
   121 is SYS_SIG_RETURN. */
__asm__(
    ".text\n"
    ".globl __quark_sig_entry\n"
    ".type __quark_sig_entry,@function\n"
    "__quark_sig_entry:\n"
    "    push %rbp\n"
    "    mov %rsp, %rbp\n"
    "    push %rbx\n"
    "    push %r12\n"
    "    mov %rdi, %rbx\n"
    "    sub __quark_fp_size(%rip), %rsp\n"
    "    and $-64, %rsp\n"
    "    mov %rsp, %r12\n"
    "    cmpb $0, __quark_fp_xsave(%rip)\n"
    "    je 1f\n"
    "    xor %eax, %eax\n"
    "    mov %rax, 512(%rsp)\n"
    "    mov %rax, 520(%rsp)\n"
    "    mov %rax, 528(%rsp)\n"
    "    mov %rax, 536(%rsp)\n"
    "    mov %rax, 544(%rsp)\n"
    "    mov %rax, 552(%rsp)\n"
    "    mov %rax, 560(%rsp)\n"
    "    mov %rax, 568(%rsp)\n"
    "    mov $-1, %eax\n"
    "    mov $-1, %edx\n"
    "    xsave (%rsp)\n"
    "    jmp 2f\n"
    "1:  fxsave (%rsp)\n"
    "2:  fninit\n"
    "    ldmxcsr __quark_mxcsr_default(%rip)\n"
    "    mov %rbx, %rdi\n"
    "    mov %r12, %rsi\n"
    "    call __quark_sig_run\n"
    "    cmpb $0, __quark_fp_xsave(%rip)\n"
    "    je 3f\n"
    "    mov $-1, %eax\n"
    "    mov $-1, %edx\n"
    "    xrstor (%r12)\n"
    "    jmp 4f\n"
    "3:  fxrstor (%r12)\n"
    "4:  mov %rbx, %rdi\n"
    "    mov $121, %eax\n"
    "    syscall\n"
    "    ud2\n"
    ".size __quark_sig_entry, .-__quark_sig_entry\n");

void __quark_sig_entry(void);
void __quark_sig_run(struct quark_sigframe *f, void *fp);

/* A timer's signal says so, and has the timer's number where a process id
   would be and its overruns where a user would be. */
#define LX_SI_TIMER (-2)
#define TIMERS 32

/* The overruns that came with the last signal of each of this program's
   timers, which is what timer_getoverrun answers: only the signal says
   them. A forked child has no timers to ask about, and exec starts over. */
static int timer_overruns[TIMERS];

/* What came with a signal, as siginfo says it: who raised it and what it
   carried — or, for a fault, where it was. */
static void fill_info(struct lx_siginfo *si, int sig, long code, unsigned long who,
                      unsigned long value, int fault) {
    __builtin_memset(si, 0, sizeof *si);
    si->si_signo = sig;
    si->si_code = (int)code;
    if (fault) {
        si->u.addr = (void *)value;
    } else {
        si->u.rt.pid = (int)(who & 0xFFFFFFFFUL);
        si->u.rt.uid = (unsigned int)(who >> 32);
        si->u.rt.value = value;
    }
    if (code == LX_SI_TIMER && (who & 0xFFFFFFFFUL) < TIMERS) {
        timer_overruns[who & 0xFFFFFFFFUL] = (int)(who >> 32);
    }
}

long __quark_syscall(long n, long a1, long a2, long a3, long a4, long a5, long a6);

/* This is a program built for Linux, whose own calls this library answers
   (`__quark_linux_start`). */
static int linux_calls;

/* Call the handler the frame names, and put back what it changed of where
   the thread was and what it holds back. */
void __quark_sig_run(struct quark_sigframe *f, void *fp) {
    int sig = (int)f->signo;
    /* A system call the program's own code made: the number is in the low
       half of what came with it, the arguments where Linux passes them —
       RDI, RSI, RDX, R10, R8, R9, at their places in the record — and the
       answer goes in RAX, which is all a call changes but RCX and R11, and
       those the record has as the instruction left them. */
    if (sig == LX_SIGSYS && f->info_code == LX_SYS_USER_DISPATCH) {
        f->regs[0] = (unsigned long)__quark_syscall((long)(unsigned int)f->info_value,
                                                    (long)f->regs[5], (long)f->regs[4],
                                                    (long)f->regs[3], (long)f->regs[9],
                                                    (long)f->regs[7], (long)f->regs[8]);
        return;
    }
    if (!(f->flags & Q_SIGINFO)) {
        ((void (*)(int))f->cookie)(sig);
        return;
    }
    struct lx_siginfo info;
    struct lx_ucontext uc;
    fill_info(&info, sig, f->info_code, f->info_who, f->info_value, f->code == 2);
    __builtin_memset(&uc, 0, sizeof uc);
    for (int i = 0; i < 18; i++) {
        uc.gregs[greg_of[i]] = f->regs[i];
    }
    uc.gregs[G_CSGSFS] = 0x33;
    uc.fpregs = fp;
    uc.sigmask[0] = f->mask;
    uc.ss_flags = (f->flags & 1) ? LX_SS_ONSTACK : 0;
    ((void (*)(int, void *, void *))f->cookie)(sig, &info, &uc);
    for (int i = 0; i < 18; i++) {
        f->regs[i] = uc.gregs[greg_of[i]];
    }
    f->mask = uc.sigmask[0];
}

/* As the program starts: where its handlers are entered, that a call a
   signal cuts short is to answer as Unix would have it, and how to keep the
   floating-point registers — XSAVE, if the kernel has turned it on, in as
   many bytes as the processor says what is turned on takes. */
void __quark_sig_start(void) {
    unsigned int a, b, c, d;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(1), "c"(0));
    if (c & (1u << 27)) {
        __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(0xD), "c"(0));
        if (b >= 576) {
            __quark_fp_size = (b + 63) & ~63UL;
            __quark_fp_xsave = 1;
        }
    }
    __syscall5(SYS_SIG_ACTION, 0, QUARK_SIG_RUN, 0, QUARK_SIG_UNIX, (unsigned long)__quark_sig_entry);
}

/* SIGSYS that was not a call, in a program that has said nothing about it
   or asked for what it does by default: what it does by default — the end
   of the program — once this handler is left. */
static void sigsys_default(int sig) {
    __syscall2(SYS_SIG_ACTION, (unsigned long)sig, QUARK_SIG_DEFAULT);
    __syscall2(SYS_SIG_RAISE, __syscall0(SYS_GETPID), (unsigned long)sig);
}

/* And one the program asked to have ignored. */
static void sigsys_ignored(int sig) {
    (void)sig;
}

/* The ELF header this library was loaded with — libc.so's, for a program
   linked to it; nothing in a program linked to this statically, whose
   headers are not loaded. */
extern const unsigned char __ehdr_start[] __attribute__((weak, visibility("hidden")));

/* A program built for Linux — it asked for musl's loader by Linux's name,
   and its loader said so (QUARK_AT_LINUX) — is running on this library as
   libc.so, and its own code may make Linux's system calls itself rather
   than ask: rustix does, in every Rust program.
   So the kernel is told that this program's calls are made from this
   library's code (SYS_SYSCALL_TRAP), and one made from anywhere else comes
   here as SIGSYS, answered by `__quark_sig_run`. A program linked to this
   statically has nowhere to say that from: one built for Linux is not
   linked to this, and one built for Quark makes its calls itself. */
void __quark_linux_start(void) {
    const unsigned char *self = __ehdr_start;
    if (!self) {
        return;
    }
    unsigned long phoff = *(const unsigned long *)(self + 32);
    unsigned short phentsize = *(const unsigned short *)(self + 54);
    unsigned short phnum = *(const unsigned short *)(self + 56);
    unsigned long from = ~0UL, to = 0;
    for (unsigned short i = 0; i < phnum; i++) {
        const unsigned char *ph = self + phoff + (unsigned long)i * phentsize;
        unsigned int type = *(const unsigned int *)ph;
        unsigned int flags = *(const unsigned int *)(ph + 4);
        unsigned long vaddr = *(const unsigned long *)(ph + 16);
        unsigned long memsz = *(const unsigned long *)(ph + 40);
        if (type != 1 /* PT_LOAD */ || !(flags & 1) /* PF_X */ || memsz == 0) {
            continue;
        }
        unsigned long start = (unsigned long)self + vaddr;
        if (start < from) {
            from = start;
        }
        if (start + memsz > to) {
            to = start + memsz;
        }
    }
    if (from >= to) {
        return;
    }
    linux_calls = 1;
    __syscall5(SYS_SIG_ACTION, LX_SIGSYS, QUARK_SIG_RUN, 0, Q_SIGINFO,
               (unsigned long)sigsys_default);
    __syscall2(SYS_SYSCALL_TRAP, from, to - from);
}

/* What this thread holds back. */
static unsigned long mask_now(void) {
    return __syscall2(SYS_SIG_MASK, QUARK_SIG_ASK, 0);
}

/* What is said about `sig` now: a copy, made without waiting for anybody. */
static struct lx_ksigaction action_of(long sig) {
    struct lx_ksigaction a;
    unsigned long k;
    for (;;) {
        unsigned long s = seq;
        __atomic_thread_fence(__ATOMIC_ACQUIRE);
        a = actions[sig];
        k = known & BIT(sig);
        __atomic_thread_fence(__ATOMIC_ACQUIRE);
        if (!(s & 1) && s == seq) {
            break;
        }
    }
    /* What was said here holds while the kernel agrees with it. Where it
       does not, the kernel says what is true: nothing was said here, or a
       handler run once (SA_RESETHAND) is gone, or exec kept an ignore. */
    unsigned long said = __syscall2(SYS_SIG_ACTION, (unsigned long)sig, QUARK_SIG_ASK);
    int handled = a.handler != LX_SIG_DFL && a.handler != LX_SIG_IGN;
    int agrees = k && (handled ? said == QUARK_SIG_RUN
                               : (said == QUARK_SIG_IGNORE) == (a.handler == LX_SIG_IGN));
    if (!agrees) {
        a.handler = said == QUARK_SIG_IGNORE ? LX_SIG_IGN : LX_SIG_DFL;
        a.flags = 0;
        a.restorer = 0;
        a.mask = 0;
    }
    return a;
}

/* In the child of a fork: whichever thread of the parent was writing is not
   here to finish. */
void __quark_sig_forked(void) {
    writing = 0;
    if (seq & 1) {
        seq++;
    }
}

long __quark_sigaction(long sig, const struct lx_ksigaction *act, struct lx_ksigaction *old,
                       unsigned long size) {
    if (sig < 1 || sig > NSIG || size != 8) {
        return -LX_EINVAL;
    }
    if (act && (sig == LX_SIGKILL || sig == LX_SIGSTOP)) {
        return -LX_EINVAL;
    }
    struct lx_ksigaction was = action_of(sig);
    if (act) {
        unsigned long r;
        if (linux_calls && sig == LX_SIGSYS &&
            (act->handler == LX_SIG_DFL || act->handler == LX_SIG_IGN)) {
            /* The calls this library answers come as SIGSYS: the kernel has
               to go on running a handler for it, whatever else the program
               wants done with one that is not a call. */
            r = __syscall5(SYS_SIG_ACTION, (unsigned long)sig, QUARK_SIG_RUN, 0, Q_SIGINFO,
                           act->handler == LX_SIG_DFL ? (unsigned long)sigsys_default
                                                      : (unsigned long)sigsys_ignored);
        } else if (act->handler == LX_SIG_DFL) {
            r = __syscall2(SYS_SIG_ACTION, (unsigned long)sig, QUARK_SIG_DEFAULT);
        } else if (act->handler == LX_SIG_IGN) {
            r = __syscall2(SYS_SIG_ACTION, (unsigned long)sig, QUARK_SIG_IGNORE);
        } else {
            unsigned long how = 0;
            if (act->flags & LX_SA_NODEFER) {
                how |= Q_NODEFER;
            }
            if (act->flags & LX_SA_RESETHAND) {
                how |= Q_RESETHAND;
            }
            if (act->flags & LX_SA_ONSTACK) {
                how |= Q_ONSTACK;
            }
            /* musl's cancellation signal asks for the call it cuts short
               to be made again, as on Linux, where its handler finds the
               thread at the instruction of the cancellable call and turns
               it aside there. Here that call is this layer's, which no
               such window holds: so the wait it ends is not made again,
               and musl, told EINTR at a cancellation point with a cancel
               pending, acts on it. */
            if ((act->flags & LX_SA_RESTART) && sig != LX_SIGCANCEL) {
                how |= Q_RESTARTS;
            }
            if (act->flags & LX_SA_SIGINFO) {
                how |= Q_SIGINFO;
            }
            r = __syscall5(SYS_SIG_ACTION, (unsigned long)sig, QUARK_SIG_RUN,
                           act->mask & ~UNBLOCKABLE, how, act->handler);
        }
        if (r == QUARK_ERR) {
            return -LX_EINVAL;
        }
        unsigned long held = __syscall2(SYS_SIG_MASK, 2 /* SIG_SETMASK */, ~0UL);
        __quark_lock(&writing);
        seq++;
        __atomic_thread_fence(__ATOMIC_RELEASE);
        actions[sig] = *act;
        known |= BIT(sig);
        __atomic_thread_fence(__ATOMIC_RELEASE);
        seq++;
        __quark_unlock(&writing);
        __syscall2(SYS_SIG_MASK, 2, held);
    }
    if (old) {
        *old = was;
    }
    return 0;
}

long __quark_sigprocmask(long how, const unsigned long *set, unsigned long *old,
                         unsigned long size) {
    if (size != 8) {
        return -LX_EINVAL;
    }
    unsigned long was;
    if (set) {
        /* SIG_BLOCK, SIG_UNBLOCK and SIG_SETMASK are the kernel's 0, 1 and
           2. What that lets through is run on the way out of this call. */
        if (how < 0 || how > 2) {
            return -LX_EINVAL;
        }
        was = __syscall2(SYS_SIG_MASK, (unsigned long)how, *set & ~UNBLOCKABLE);
    } else {
        was = mask_now();
    }
    if (old) {
        *old = was;
    }
    return 0;
}

long __quark_sigpending(unsigned long *set, unsigned long size) {
    if (size != 8 || !set) {
        return -LX_EINVAL;
    }
    *set = __syscall2(SYS_SIG_MASK, QUARK_SIG_MASK_PENDING, 0);
    return 0;
}

/* sigsuspend, and pause with no mask: wait under `mask` until a handler has
   run in this thread. */
long __quark_sigsuspend(const unsigned long *mask, unsigned long size) {
    if (mask && size != 8) {
        return -LX_EINVAL;
    }
    unsigned long under = (mask ? *mask : mask_now()) & ~UNBLOCKABLE;
    while (__syscall2(SYS_SIG_MASK, QUARK_SIG_MASK_WAIT, under) == QUARK_AGAIN) {
    }
    return -LX_EINTR;
}

/* sigtimedwait: take one of `set` without running its handler. */
long __quark_sigtimedwait(const unsigned long *set, void *info, const long *timeout,
                          unsigned long size) {
    if (size != 8 || !set) {
        return -LX_EINVAL;
    }
    unsigned long deadline = 0;
    if (timeout) {
        deadline = quark_now() + quark_nanos((unsigned long)timeout[0], (unsigned long)timeout[1]);
    }
    for (;;) {
        unsigned long span = ~0UL;
        if (timeout) {
            unsigned long now = quark_now();
            span = now < deadline ? quark_span(deadline - now) : 0;
        }
        /* What came with it, as a handler's record ends. */
        unsigned long came[3] = {0, 0, 0};
        unsigned long r = __syscall4(SYS_SIG_WAIT, *set, span, (unsigned long)came, 1);
        if (r >= 1 && r <= NSIG) {
            if (info) {
                fill_info(info, (int)r, (long)came[0], came[1], came[2], 0);
            }
            return (long)r;
        }
        if (r == 0) {
            return -LX_EAGAIN;
        }
        if (r != QUARK_AGAIN) {
            return -LX_EINTR;
        }
    }
}

/* POSIX's timers: the kernel's (SYS_PTIMER), which raise a signal. What
   musl hands timer_create is Linux's sigevent as the kernel takes it: a
   signal for the program, one for one thread (SIGEV_THREAD_ID, which musl
   builds SIGEV_THREAD on: a thread of its own waits for the signal and
   calls the function), or none. None at all is SIGALRM carrying the
   timer's own number. */
#define LX_SIGEV_SIGNAL    0
#define LX_SIGEV_NONE      1
#define LX_SIGEV_THREAD_ID 4
#define LX_SIGALRM         14
#define LX_TIMER_ABSTIME   1
#define LX_CLOCK_REALTIME  0
#define LX_CLOCK_MONOTONIC 1
#define LX_CLOCK_BOOTTIME  7

struct lx_sigevent {
    unsigned long sigev_value;
    int sigev_signo;
    int sigev_notify;
    int sigev_tid;
};

long __quark_timer_create(long clock, const void *sevp, int *id) {
    if (!id) {
        return -LX_EFAULT;
    }
    if (clock != LX_CLOCK_REALTIME && clock != LX_CLOCK_MONOTONIC && clock != LX_CLOCK_BOOTTIME) {
        /* The processor-time clocks among them: nothing counts those down. */
        return -LX_EINVAL;
    }
    unsigned long signo = LX_SIGALRM | 0x100, value = 0, task = 0;
    if (sevp) {
        const struct lx_sigevent *e = sevp;
        value = e->sigev_value;
        signo = (unsigned long)e->sigev_signo;
        switch (e->sigev_notify) {
        case LX_SIGEV_NONE:
            signo = 0;
            break;
        case LX_SIGEV_THREAD_ID:
            if (e->sigev_tid <= 0) {
                return -LX_EINVAL;
            }
            task = (unsigned long)e->sigev_tid;
            /* fall through */
        case LX_SIGEV_SIGNAL:
            if (signo < 1 || signo > NSIG) {
                return -LX_EINVAL;
            }
            break;
        default:
            return -LX_EINVAL;
        }
    }
    unsigned long r = __syscall5(SYS_PTIMER, 0, (unsigned long)clock, signo, value, task);
    if (r == QUARK_ERR) {
        /* A thread of another program, or as many timers as there may be. */
        return task ? -LX_EINVAL : -LX_EAGAIN;
    }
    timer_overruns[r % TIMERS] = 0;
    *id = (int)r;
    return 0;
}

/* Linux's itimerspec: the interval, then the value, each seconds and
   nanoseconds. */
static unsigned long timespec_ns(const long *ts) {
    return quark_nanos((unsigned long)ts[0], (unsigned long)ts[1]);
}

static int timespec_bad(const long *ts) {
    return ts[0] < 0 || ts[1] < 0 || ts[1] >= 1000000000L;
}

static void timespec_of(long *ts, unsigned long ns) {
    ts[0] = (long)(ns / 1000000000UL);
    ts[1] = (long)(ns % 1000000000UL);
}

long __quark_timer_settime(long id, long flags, const void *new_value, void *old_value) {
    const long *nv = new_value;
    if (!nv) {
        return -LX_EFAULT;
    }
    if (timespec_bad(nv) || timespec_bad(nv + 2)) {
        return -LX_EINVAL;
    }
    unsigned long first = timespec_ns(nv + 2), every = timespec_ns(nv);
    int absolute = (flags & LX_TIMER_ABSTIME) != 0;
    unsigned long when = !first ? 0 : absolute ? first : quark_span(first);
    unsigned long was[2] = {0, 0};
    unsigned long timer = (unsigned long)(unsigned int)id | (absolute ? 1UL << 32 : 0);
    if (__syscall5(SYS_PTIMER, 1, timer, when, every ? quark_span(every) : 0, (unsigned long)was) ==
        QUARK_ERR) {
        return -LX_EINVAL;
    }
    if (old_value) {
        long *ov = old_value;
        timespec_of(ov, was[1]);
        timespec_of(ov + 2, was[0]);
    }
    return 0;
}

long __quark_timer_gettime(long id, void *curr_value) {
    if (!curr_value) {
        return -LX_EFAULT;
    }
    unsigned long stands[2] = {0, 0};
    if (__syscall3(SYS_PTIMER, 2, (unsigned long)(unsigned int)id, (unsigned long)stands) == QUARK_ERR) {
        return -LX_EINVAL;
    }
    long *cv = curr_value;
    timespec_of(cv, stands[1]);
    timespec_of(cv + 2, stands[0]);
    return 0;
}

long __quark_timer_getoverrun(long id) {
    if (__syscall3(SYS_PTIMER, 2, (unsigned long)(unsigned int)id, 0) == QUARK_ERR) {
        return -LX_EINVAL;
    }
    return timer_overruns[(unsigned long)id % TIMERS];
}

long __quark_timer_delete(long id) {
    return __syscall2(SYS_PTIMER, 3, (unsigned long)(unsigned int)id) == QUARK_ERR ? -LX_EINVAL : 0;
}

/* signalfd and signalfd4: a descriptor read for signals (SYS_SIGNAL_FD),
   whose records the kernel lays out as Linux's signalfd_siginfo — a read of
   one is a read like any other here. A descriptor of -1 is a new one;
   another is one of the program's to read for a new set. SFD_NONBLOCK is
   the descriptor's, as O_NONBLOCK is; SFD_CLOEXEC means nothing here, as
   for a pipe or a counter. */
#define LX_SFD_NONBLOCK 04000
#define LX_SFD_CLOEXEC  02000000

long __quark_signalfd(long fd, const unsigned long *mask, unsigned long size, long flags) {
    if (size != 8 || (flags & ~(LX_SFD_NONBLOCK | LX_SFD_CLOEXEC))) {
        return -LX_EINVAL;
    }
    if (!mask) {
        return -LX_EFAULT;
    }
    unsigned long r = __syscall2(SYS_SIGNAL_FD, fd < 0 ? ~0UL : (unsigned long)fd, *mask);
    if (r == QUARK_ERR) {
        if (fd < 0) {
            return -LX_EMFILE;
        }
        return __syscall1(SYS_FD_KIND, (unsigned long)fd) == QUARK_ERR ? -LX_EBADF : -LX_EINVAL;
    }
    if (fd < 0 && (flags & LX_SFD_NONBLOCK)) {
        __quark_fd_set_nonblock((long)r, 1);
    }
    return (long)r;
}

/* sigaltstack: the stack for handlers that ask for one, this thread's. */
struct lx_stack {
    void *ss_sp;
    int ss_flags;
    unsigned long ss_size;
};

long __quark_sigaltstack(const void *new_stack, void *old_stack) {
    const struct lx_stack *ss = new_stack;
    struct lx_stack *old = old_stack;
    unsigned long was[2] = {0, 0};
    unsigned long base = 0, size = 0;
    if (ss) {
        if (ss->ss_flags & ~LX_SS_DISABLE) {
            return -LX_EINVAL;
        }
        if (!(ss->ss_flags & LX_SS_DISABLE)) {
            if (ss->ss_size < LX_MINSIGSTKSZ) {
                return -LX_ENOMEM;
            }
            base = (unsigned long)ss->ss_sp;
            size = ss->ss_size;
        }
    }
    __syscall3(SYS_SIG_STACK, ~0UL, 0, (unsigned long)was);
    /* Not while a handler is running on it. */
    unsigned long here = (unsigned long)__builtin_frame_address(0);
    int on_it = was[1] && here > was[0] && here <= was[0] + was[1];
    if (ss && on_it) {
        return -LX_EPERM;
    }
    if (ss && __syscall3(SYS_SIG_STACK, base, size, 0) == QUARK_ERR) {
        return -LX_EINVAL;
    }
    if (old) {
        old->ss_sp = (void *)was[0];
        old->ss_size = was[1];
        old->ss_flags = !was[1] ? LX_SS_DISABLE : on_it ? LX_SS_ONSTACK : 0;
    }
    return 0;
}

/* kill. A signal is said to a program, which is named by its process id. One
   that has ended and not been collected is still there to be named, and the
   kernel says yes and does nothing; an id that names nothing is ESRCH, and
   stays so, since no other program is ever given it. */
long __quark_kill(long pid, long sig) {
    if (sig < 0 || sig > NSIG) {
        return -LX_EINVAL;
    }
    if (pid == -1) {
        /* Everything this program may signal. Not something to do because a
           script got a variable wrong. */
        return -LX_EPERM;
    }
    if (pid <= 0) {
        /* A process group: the caller's own, or the one named. For a shell
           this is a job — every program of a pipeline at once. */
        unsigned long r = __syscall3(SYS_SIG_RAISE, (unsigned long)-pid, (unsigned long)sig,
                                     QUARK_RAISE_GROUP);
        return r == QUARK_ERR ? -LX_ESRCH : r == QUARK_NOT_ALLOWED ? -LX_EPERM : 0;
    }
    unsigned long r = __syscall3(SYS_SIG_RAISE, (unsigned long)pid, (unsigned long)sig, QUARK_RAISE_BY_PID);
    if (r == QUARK_WOULD_BLOCK) {
        /* A real-time signal with as many of it waiting as can. */
        return -LX_EAGAIN;
    }
    if (r != QUARK_ERR) {
        return 0;
    }
    /* The kernel says no one way, for a process that is not there and for
       one that is somebody else's. Which it was is what the caller is told
       apart by — `kill -0` asks exactly that — and a process that has a
       group is a process that is there. */
    return __syscall2(SYS_PGROUP, QUARK_PGROUP_GET, (unsigned long)pid) == QUARK_ERR ? -LX_ESRCH
                                                                                     : -LX_EPERM;
}

/* tkill and tgkill: a signal for one thread, run in that thread — for the
   caller's own, before the call returns, which is what `raise` and `abort`
   depend on. */
long __quark_tkill(long tid, long sig) {
    if (sig < 0 || sig > NSIG || tid <= 0) {
        return -LX_EINVAL;
    }
    unsigned long r = __syscall3(SYS_SIG_RAISE, (unsigned long)tid, (unsigned long)sig, QUARK_RAISE_THREAD);
    return r == QUARK_ERR ? -LX_ESRCH : r == QUARK_WOULD_BLOCK ? -LX_EAGAIN : 0;
}

/* rt_sigqueueinfo, and with a thread rt_tgsigqueueinfo: a signal carrying
   the value in the record the program filled — the record's alone. Who
   raised it, and as what, are the kernel's to say. A real-time signal waits
   behind one of its number, and EAGAIN is the kernel saying it cannot. */
long __quark_sigqueue(long pid, long tid, long sig, const void *info) {
    if (sig < 0 || sig > NSIG || !info) {
        return -LX_EINVAL;
    }
    unsigned long value = ((const struct lx_siginfo *)info)->u.rt.value;
    unsigned long r;
    if (tid >= 0) {
        if (tid == 0) {
            return -LX_EINVAL;
        }
        r = __syscall4(SYS_SIG_QUEUE, (unsigned long)tid, (unsigned long)sig, value, QUARK_RAISE_THREAD);
        return r == QUARK_ERR ? -LX_ESRCH : r == QUARK_WOULD_BLOCK ? -LX_EAGAIN : 0;
    }
    if (pid <= 0) {
        return -LX_ESRCH;
    }
    r = __syscall4(SYS_SIG_QUEUE, (unsigned long)pid, (unsigned long)sig, value, QUARK_RAISE_BY_PID);
    if (r == QUARK_WOULD_BLOCK) {
        return -LX_EAGAIN;
    }
    if (r != QUARK_ERR) {
        return 0;
    }
    return __syscall2(SYS_PGROUP, QUARK_PGROUP_GET, (unsigned long)pid) == QUARK_ERR ? -LX_ESRCH
                                                                                     : -LX_EPERM;
}

/* A write found nobody at the other end. */
void __quark_sig_pipe(void) {
    __quark_tkill((long)self(), LX_SIGPIPE);
}
