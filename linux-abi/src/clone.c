/* Making a thread, when the kernel does not do it Linux's way.
 *
 * Linux's `clone` has the child *return from the same system call* on a new
 * stack, which is why musl's `__clone` is assembly: the child continues at the
 * instruction after `syscall` with a different stack pointer. Quark starts a
 * task at an entry point instead, and cannot resume something that never ran.
 *
 * So musl's `__clone` is replaced by a tail call into here — its seven
 * arguments are already in the registers a C function wants — and this does
 * the whole thing: create a task in the address space we are already in, plant
 * the thread-pointer and the function on its stack, and start it at a
 * trampoline that picks them up.
 *
 * What the child needs and one register cannot carry: the TLS pointer, the
 * function, and its argument. The argument travels in RDI, which is what
 * `sys_task_start_arg` fills; the other two are written onto the child's own
 * stack, below the top musl gave us, where nothing else will look.
 */

#include <quark/syscall.h>

#include "abi.h"

/* Linux's clone flags, of which only these mean anything here. */
#define CLONE_VM      0x00000100
#define CLONE_VFORK   0x00004000
#define CLONE_THREAD  0x00010000
#define CLONE_SETTLS  0x00080000
#define CLONE_CHILD_CLEARTID 0x00200000

long __quark_set_fs(unsigned long tp);

/* Defined in clone-entry.s. Starts with RDI = the thread function's argument
   and RSP pointing at [tls][func]. */
void __quark_thread_entry(void);

/* Called by the trampoline when the thread function returns. */
void __quark_thread_exit(int code);

void __quark_thread_exit(int code) {
    __syscall1(SYS_EXIT_CODE, (unsigned long)code);
    for (;;) { }
}

long __quark_syscall(long n, long a1, long a2, long a3, long a4, long a5, long a6);

/* A task that was made and will not be started: ended, and collected, so
   that its place among the system's tasks comes back. It has no memory and
   has run nothing; a wait that does not wait finds it at once. */
static void discard(unsigned long tid) {
    __syscall1(SYS_TASK_KILL, tid);
    __syscall2(SYS_WAIT_FOR, tid, QUARK_WAIT_NOW);
}

/* The end of a detached thread, on the stack kept for it (clone-entry.s):
   its own stack is given back, as any mapping is, and then it is gone.
   Linux's munmap is call 11. musl has blocked every signal by now, so
   nothing is run on the way out of it. */
void __quark_unmap_and_exit(void *base, unsigned long size);
void __quark_unmap_and_exit(void *base, unsigned long size) {
    __quark_syscall(11, (long)base, (long)size, 0, 0, 0, 0);
    __syscall1(SYS_EXIT_CODE, 0);
    for (;;) { }
}

long __quark_clone(int (*func)(void *), void *stack, int flags, void *arg,
                   int *ptid, void *tls, int *ctid) {
    /* A child with a *copy* of the address space rather than a share of it is
       a process, and the kernel makes one in a single call: it copies the
       caller's pages, its descriptors and its capabilities, and the child
       returns from that call rather than starting at an entry point. So
       nothing of the thread path below applies to it.
       
       musl's `fork` sends SIGCHLD and nothing else; anything asking for a new
       process *and* something clever — a shared file table, a stopped child —
       is asking for a Linux this is not. */
    if (!(flags & CLONE_VM)) {
        if (flags & (CLONE_THREAD | CLONE_SETTLS)) {
            return -LX_ENOSYS;
        }
        return __quark_fork();
    }
    if (!(flags & CLONE_THREAD)) {
        /* A child that borrows its parent's memory, with the parent held
           until the child has become another program or gone: vfork's
           bargain, and what musl makes `posix_spawn` of. The point of it on
           Linux is not to copy an address space that is about to be thrown
           away. Here the child is given a copy all the same, as `vfork`
           is: the kernel has one way to make a process.

           What a caller could see is whatever the child wrote to memory,
           which it will not find in its own. musl's does not look: its
           child says how the exec went down a pipe. The function is run
           where it stands, on the child's copy of this stack — there is
           nobody else on it — and the child ends with what it returns. */
        if (!(flags & CLONE_VFORK) || !func) {
            return -LX_ENOSYS;
        }
        long pid = __quark_fork();
        if (pid != 0) {
            return pid;
        }
        __syscall1(SYS_EXIT_PROGRAM, (unsigned long)(func(arg) & 0xFF));
        for (;;) { }
    }
    if (!func || !stack) {
        return -LX_EINVAL;
    }

    unsigned long cr3 = __syscall0(SYS_ADDRSPACE_SELF);
    if (cr3 == QUARK_ERR) {
        return -LX_EAGAIN;
    }
    unsigned long tid = __syscall0(SYS_TASK_CREATE);
    if (tid == QUARK_ERR) {
        return -LX_EAGAIN;
    }

    /* Sixteen-aligned, and far enough below musl's top that the two words
       planted here are inside the mapping rather than one past it. The kernel
       aligns what it is given and subtracts eight, so the child wakes with RSP
       exactly at the first of them. */
    unsigned long top = ((unsigned long)stack & ~15UL) - 48;
    unsigned long *slot = (unsigned long *)top;
    slot[-1] = (flags & CLONE_SETTLS) ? (unsigned long)tls : 0;
    slot[0] = (unsigned long)func;
    /* CLONE_CHILD_CLEARTID, which musl depends on to release the thread-list
       lock it holds through its own exit — and which is also how the kernel
       knows this is a thread that will be joined and not a child that will
       be waited for. Said here, for the thread, before it is started: left
       to the thread to say when it first ran, there was a moment in which
       its creator had a child, and a `waitpid` in that moment was told to
       wait for something no wait would ever be given. */
    if ((flags & CLONE_CHILD_CLEARTID) &&
        __syscall2(SYS_SET_CLEAR_TID, (unsigned long)ctid, tid) == QUARK_ERR) {
        discard(tid);
        return -LX_EAGAIN;
    }

    /* musl reads the new thread's id out of *ptid, and it must be there before
       the thread can run — afterwards is a race with the thread exiting. */
    if (ptid) {
        *ptid = (int)tid;
    }
    unsigned long started = __syscall5(SYS_TASK_START_ARG, tid,
                                       (unsigned long)__quark_thread_entry,
                                       top, cr3, (unsigned long)arg);
    if (started == QUARK_ERR) {
        discard(tid);
        return -LX_EAGAIN;
    }
    return (long)tid;
}

/* CLONE_CHILD_CLEARTID turned out not to be optional, and the comment that
   used to be here said it was.
 *
 * musl's `__pthread_exit` takes the thread-list lock and never unlocks it. Its
 * own comment explains why: "This change will not be visible until the lock is
 * released, which only happens after SYS_exit has been called, via the exit
 * futex address pointing at the lock." The lock *is* the word at `ctid`, and
 * the kernel clearing it on exit is what publishes the dead thread's removal
 * from the list. Without it the list stays locked by a task that no longer
 * exists, and the next thread to touch it — a joiner, or the process
 * exiting — waits for ever. */
