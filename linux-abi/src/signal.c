/* Signals, for a libc that thinks it is talking to Linux.
 *
 * On Linux the kernel runs a handler: it stops the program wherever it is,
 * builds a frame on its stack and resumes it somewhere else. Quark's kernel
 * runs none. It knows what a program has said about each signal — nothing,
 * ignore it, or "I have a handler" — and for the first two it does what is
 * to be done, which is nothing or the end of the program. For the third it
 * tells the program, and the program runs its own handler: here.
 *
 * It tells it twice. A word of this program's memory is set, and that is
 * looked at on the way out of every system call. And because a program
 * waiting for a key is making no system call, the waits a program sits in —
 * a read of a terminal, a poll, a sleep — end early and say why. Either way
 * the signals waiting are taken from the kernel and the handlers called from
 * here, as ordinary functions on the stack of whatever was being done.
 *
 * So a handler runs at a system-call boundary and nowhere else, which is a
 * subset of where Linux could run it: nothing a handler may do on Linux is
 * wrong here, a handler may still leave by `longjmp`, and a program that
 * computes for ever without a call is not interrupted. What is not here at
 * all: a mask for a signal with no handler (it does what it does at once),
 * anything sent when a child ends or a timer runs out, and a thread a signal
 * is aimed at — the first thread to look runs the handler.
 */

#include <quark/syscall.h>

#include "abi.h"

#define NSIG 64
#define BIT(sig) (1UL << ((sig) - 1))

#define LX_SIGKILL 9
#define LX_SIGPIPE 13
#define LX_SIGSTOP 19

#define LX_SIG_DFL 0UL
#define LX_SIG_IGN 1UL

#define LX_SA_SIGINFO   4UL
#define LX_SA_RESTART   0x10000000UL
#define LX_SA_NODEFER   0x40000000UL
#define LX_SA_RESETHAND 0x80000000UL

/* The two a program may not refuse. */
#define UNBLOCKABLE (BIT(LX_SIGKILL) | BIT(LX_SIGSTOP))

/* What a program has said about each signal. */
static struct lx_ksigaction actions[NSIG + 1];
/* Which of those it has said since it started. One it has not is as it was
   started: nothing said — or ignored, if whatever exec'd it was ignoring the
   signal, which the kernel kept and this program's memory did not. */
static unsigned long known;
/* Raised, with a handler, and not yet run. */
static unsigned long pending;
/* The mask. One for the program rather than one a thread: the C library
   blocks everything around the places it must not be interrupted and puts
   back what it found, and that comes out the same either way. */
static unsigned long blocked;
/* Set to 1 by the kernel when a signal with a handler arrives. */
static volatile unsigned int hint;
static int kernel_knows_where;

static unsigned long self(void) {
    return __syscall0(SYS_GETPID);
}

/* The signals that do nothing to a program that has said nothing. */
static int harmless(long sig) {
    return sig == 17 || sig == 23 || sig == 28 || (sig >= 18 && sig <= 22);
}

/* Take what the kernel has for this program, and tell it where the word is. */
static void take(void) {
    hint = 0;
    unsigned long got = __syscall1(SYS_SIG_TAKE, (unsigned long)&hint);
    kernel_knows_where = 1;
    if (got != QUARK_ERR && got) {
        __sync_fetch_and_or(&pending, got);
    }
}

static struct lx_ksigaction *action_of(long sig) {
    if (!(known & BIT(sig))) {
        unsigned long was = __syscall2(SYS_SIG_ACTION, (unsigned long)sig, QUARK_SIG_ASK);
        actions[sig].handler = was == QUARK_SIG_IGNORE ? LX_SIG_IGN : LX_SIG_DFL;
        actions[sig].flags = 0;
        actions[sig].restorer = 0;
        actions[sig].mask = 0;
        known |= BIT(sig);
    }
    return &actions[sig];
}

/* Call the handler for `sig`. Returns 1 if a call it cut short should say so
   rather than be made again. */
static int run(long sig) {
    struct lx_ksigaction a = *action_of(sig);
    if (a.handler == LX_SIG_IGN) {
        return 0;
    }
    if (a.handler == LX_SIG_DFL) {
        /* It had a handler when it was raised and has none now. What the
           signal does is the kernel's to do. */
        if (!harmless(sig)) {
            __syscall2(SYS_SIG_RAISE, self(), (unsigned long)sig);
        }
        return 0;
    }
    unsigned long saved = blocked;
    unsigned long during = saved | a.mask;
    if (!(a.flags & LX_SA_NODEFER)) {
        during |= BIT(sig);
    }
    blocked = during & ~UNBLOCKABLE;
    if (a.flags & LX_SA_RESETHAND) {
        actions[sig].handler = LX_SIG_DFL;
        __syscall2(SYS_SIG_ACTION, (unsigned long)sig, QUARK_SIG_DEFAULT);
    }
    if (a.flags & LX_SA_SIGINFO) {
        /* A siginfo_t and a ucontext_t, both empty but for the number: there
           is no sender to name and no interrupted frame to describe. */
        unsigned long info[16];
        unsigned long context[128];
        for (int i = 0; i < 16; i++) {
            info[i] = 0;
        }
        for (int i = 0; i < 128; i++) {
            context[i] = 0;
        }
        info[0] = (unsigned long)sig; /* si_signo, and si_errno of 0 above it */
        ((void (*)(int, void *, void *))a.handler)((int)sig, info, context);
    } else {
        ((void (*)(int))a.handler)((int)sig);
    }
    /* A handler that left by longjmp never gets here, and the mask it left
       with is its program's to put right — which is what sigsetjmp saves it
       for. */
    blocked = saved;
    return !(a.flags & LX_SA_RESTART);
}

/* Is there anything to do on the way out of a call? */
int __quark_sig_due(void) {
    return hint || (pending & ~blocked);
}

/* Run every handler that may run now. Nonzero if one of them wants the call
   it interrupted to fail with EINTR. */
int __quark_sig_deliver(void) {
    int interrupts = 0;
    if (hint) {
        take();
    }
    for (;;) {
        unsigned long ready = pending & ~blocked;
        if (!ready) {
            break;
        }
        long sig = __builtin_ctzl(ready) + 1;
        __sync_fetch_and_and(&pending, ~BIT(sig));
        interrupts |= run(sig);
    }
    return interrupts;
}

/* The kernel ended a wait for a signal. Take it and run what may run; 1 if
   the call should say EINTR, 0 if it should wait again. */
int __quark_sig_interrupted(void) {
    take();
    return __quark_sig_deliver();
}

/* In the child of a fork: what was waiting was the parent's. */
void __quark_sig_forked(void) {
    pending = 0;
    hint = 0;
}

long __quark_sigaction(long sig, const struct lx_ksigaction *act, struct lx_ksigaction *old,
                       unsigned long size) {
    if (sig < 1 || sig > NSIG || size != 8) {
        return -LX_EINVAL;
    }
    struct lx_ksigaction *now = action_of(sig);
    struct lx_ksigaction was = *now;
    if (act) {
        if (sig == LX_SIGKILL || sig == LX_SIGSTOP) {
            return -LX_EINVAL;
        }
        unsigned long how = act->handler == LX_SIG_DFL   ? QUARK_SIG_DEFAULT
                            : act->handler == LX_SIG_IGN ? QUARK_SIG_IGNORE
                                                         : QUARK_SIG_HANDLE;
        /* Before the kernel is told there is a handler, it has to know where
           to say so. */
        if (how == QUARK_SIG_HANDLE && !kernel_knows_where) {
            take();
        }
        *now = *act;
        __syscall2(SYS_SIG_ACTION, (unsigned long)sig, how);
        /* A signal waiting for a handler that is no longer there. */
        if (how == QUARK_SIG_IGNORE || (how == QUARK_SIG_DEFAULT && harmless(sig))) {
            __sync_fetch_and_and(&pending, ~BIT(sig));
        }
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
    unsigned long was = blocked;
    if (set) {
        unsigned long m = *set & ~UNBLOCKABLE;
        switch (how) {
        case 0: blocked = was | m; break;  /* SIG_BLOCK */
        case 1: blocked = was & ~m; break; /* SIG_UNBLOCK */
        case 2: blocked = m; break;        /* SIG_SETMASK */
        default: return -LX_EINVAL;
        }
    }
    if (old) {
        *old = was;
    }
    /* What that let through runs on the way out of this call. */
    return 0;
}

/* The mask, for a call that waits under a different one (ppoll, pselect):
   put `mask` in place and return what was there. */
unsigned long __quark_sig_swap_mask(unsigned long mask) {
    unsigned long was = blocked;
    blocked = mask & ~UNBLOCKABLE;
    return was;
}

long __quark_sigpending(unsigned long *set, unsigned long size) {
    if (size != 8 || !set) {
        return -LX_EINVAL;
    }
    if (hint) {
        take();
    }
    *set = pending & blocked;
    return 0;
}

/* Sleep until a signal arrives or `ticks` pass. The kernel ends this sleep
   for a signal with a handler, including one raised a moment before it
   began. */
static void doze(unsigned long ticks) {
    struct quark_msg m;
    __syscall3(SYS_RECV_TIMEOUT, self(), (unsigned long)&m, ticks);
}

/* sigsuspend, and pause with no mask: wait, under `mask`, until a handler has
   run. */
long __quark_sigsuspend(const unsigned long *mask, unsigned long size) {
    if (mask && size != 8) {
        return -LX_EINVAL;
    }
    unsigned long saved = blocked;
    if (mask) {
        blocked = *mask & ~UNBLOCKABLE;
    }
    for (;;) {
        if (hint) {
            take();
        }
        if (pending & ~blocked) {
            __quark_sig_deliver();
            break;
        }
        doze(0xFFFFFFFFUL);
    }
    blocked = saved;
    return -LX_EINTR;
}

/* sigtimedwait: take one of `set` without running its handler. Only a signal
   with a handler waits to be taken; one with none has already done what it
   does. */
long __quark_sigtimedwait(const unsigned long *set, void *info, const long *timeout,
                          unsigned long size) {
    if (size != 8 || !set) {
        return -LX_EINVAL;
    }
    unsigned long want = *set;
    unsigned long deadline = 0;
    if (timeout) {
        deadline = __syscall0(SYS_TICKS) + (unsigned long)timeout[0] * 100 +
                   ((unsigned long)timeout[1] + 9999999UL) / 10000000UL;
    }
    for (;;) {
        if (hint) {
            take();
        }
        unsigned long have = pending & want;
        if (have) {
            long sig = __builtin_ctzl(have) + 1;
            __sync_fetch_and_and(&pending, ~BIT(sig));
            if (info) {
                unsigned long *words = info;
                for (int i = 1; i < 16; i++) {
                    words[i] = 0;
                }
                words[0] = (unsigned long)sig;
            }
            return sig;
        }
        /* Something else arrived, with a handler that may run: that is an
           interruption. */
        if (pending & ~blocked) {
            __quark_sig_deliver();
            return -LX_EINTR;
        }
        unsigned long left = 0xFFFFFFFFUL;
        if (timeout) {
            unsigned long now = __syscall0(SYS_TICKS);
            if (now >= deadline) {
                return -LX_EAGAIN;
            }
            left = deadline - now;
        }
        doze(left);
    }
}

/* kill. A signal is said to a program, and a process id here is a task's:
   the program is whichever one that task belongs to. */
long __quark_kill(long pid, long sig) {
    if (sig < 0 || sig > NSIG) {
        return -LX_EINVAL;
    }
    if (pid == -1) {
        /* Everything this program may signal. Not something to do because a
           script got a variable wrong. */
        return -LX_EPERM;
    }
    /* 0 and a negative number name a process group, and there are none: a
       group is its leader. */
    unsigned long tid = pid == 0 ? self() : (unsigned long)(pid < 0 ? -pid : pid);
    if (__syscall2(SYS_SIG_RAISE, tid, (unsigned long)sig) != QUARK_ERR) {
        return 0;
    }
    unsigned long info = __syscall1(SYS_TASK_INFO, tid);
    if (info == QUARK_ERR) {
        return -LX_ESRCH;
    }
    /* Ended and not yet collected: there is nothing left to tell, and saying
       so is success. */
    return (info & 0xF) == 3 ? 0 : -LX_EPERM;
}

/* tkill and tgkill: a signal for one thread. For the caller's own it is
   exact — the handler runs here, in this thread, before the call returns,
   which is what `raise` and `abort` depend on. */
long __quark_tkill(long tid, long sig) {
    if (sig < 0 || sig > NSIG || tid <= 0) {
        return -LX_EINVAL;
    }
    if ((unsigned long)tid != self()) {
        /* Another thread of this program, or another program. The first is
           wrong for the two signals the C library sends a thread to cancel
           it or to run something in it: whichever thread looked first would
           be the one cancelled. */
        if (sig >= 32 && sig <= 34) {
            return -LX_ENOSYS;
        }
        return __quark_kill(tid, sig);
    }
    if (sig == 0) {
        return 0;
    }
    unsigned long handler = action_of(sig)->handler;
    if (handler == LX_SIG_IGN) {
        return 0;
    }
    if (handler == LX_SIG_DFL) {
        if (!harmless(sig)) {
            /* The end of this program, and the kernel's to carry out: a
               program cannot exit with a signal's status by asking to. */
            __syscall2(SYS_SIG_RAISE, (unsigned long)tid, (unsigned long)sig);
        }
        return 0;
    }
    /* Run on the way out of this call. */
    __sync_fetch_and_or(&pending, BIT(sig));
    return 0;
}

/* A write found nobody at the other end. */
void __quark_sig_pipe(void) {
    __quark_tkill((long)self(), LX_SIGPIPE);
}
