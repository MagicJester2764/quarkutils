/* The ways into the kernel and out of it that a program can bend.
 *
 * A system call goes back with `sysret`, to the address after the call. On
 * Intel's processors a `sysret` to an address that is not canonical faults
 * in the kernel — after `swapgs`, with the program's stack pointer already
 * loaded — and the kernel takes a fault on a stack the program chose
 * (CVE-2012-0217). Two ways led there. A `syscall` in the last two bytes
 * below 2^47 goes back to 2^47, and that page could be mapped. And a
 * program could be started (SYS_EXEC_SPACE) at an address it named, which
 * was checked against the bottom of the user half and not its top. Two of a
 * program's flags came into the kernel with a call, too: TF, which
 * single-steps — and a trap in the kernel is a kernel fault, which halts the
 * machine — and NT, which makes an `iretq` fault in the kernel, as the
 * return from a signal handler is one.
 *
 * The last page below 2^47 cannot be mapped. A program cannot be started at
 * 2^47, or at an address of the kernel's: the `iretq` into it faulted in
 * ring 0 on an AMD processor too, and halted the machine. A call made with
 * TF set traps in the program, after it — before, the trap came at the
 * kernel's first instruction, on the program's stack, and was a double
 * fault. A handler that returns with NT set returns: before, it halted the
 * machine.
 *
 * Exits 0 only if every check holds. With an argument, one of them —
 * `top`, `exec`, `tf` or `nt` — since a kernel that lets the last three
 * through faults in ring 0, and halts.
 */
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#include <quark/syscall.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed = 1;
    }
}

/* In a child: whether starting a program at `entry` is refused, which is the
   child going on to exit 0. */
static int refused_at(unsigned long entry) {
    pid_t c = fork();
    if (c == 0) {
        unsigned long cr3 = __syscall0(SYS_ADDRSPACE_CREATE);
        if (cr3 == QUARK_ERR) {
            _exit(2);
        }
        unsigned long r = __syscall3(SYS_EXEC_SPACE, cr3, entry, 0x700000000000UL);
        __syscall1(SYS_ADDRSPACE_DESTROY, cr3);
        _exit(r == QUARK_ERR ? 0 : 3);
    }
    int status = 0;
    return c > 0 && waitpid(c, &status, 0) == c && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

static volatile int traps;

static void on_trap(int sig) {
    (void)sig;
    traps++;
}

static void single_step_into_a_call(void) {
    signal(SIGTRAP, on_trap);
    unsigned long nr = SYS_GETPID;
    __asm__ volatile(
        "pushfq\n\t"
        "orq $0x100, (%%rsp)\n\t"
        "popfq\n\t"
        "syscall\n\t"
        "nop\n\t"
        : "+a"(nr)
        :
        : "rcx", "r11", "memory");
    check("a call made single-stepping traps in the program, after it", traps >= 1);
}

static volatile int handled;

static void sets_nt(int sig) {
    (void)sig;
    handled = 1;
    __asm__ volatile(
        "pushfq\n\t"
        "orq $0x4000, (%%rsp)\n\t"
        "popfq\n\t"
        :
        :
        : "memory");
}

static void return_with_nt(void) {
    signal(SIGUSR1, sets_nt);
    raise(SIGUSR1);
    check("a handler that returns with NT set returns", handled);
}

static void top_page(void) {
    unsigned long r = __syscall3(SYS_MAP_ANON, 0x7ffffffff000UL, 1, 0);
    check("the last page below 2^47 cannot be mapped", r == QUARK_ERR);
}

static void exec_entries(void) {
    check("a program cannot be started at 2^47, which is no address", refused_at(0x800000000000UL));
    check("nor at an address of the kernel's", refused_at(0xffff800000000000UL));
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("kernelentry:\n");
    const char *one = argc > 1 ? argv[1] : "";
    if (!*one || strcmp(one, "top") == 0) {
        top_page();
    }
    if (!*one || strcmp(one, "exec") == 0) {
        exec_entries();
    }
    if (!*one || strcmp(one, "tf") == 0) {
        single_step_into_a_call();
    }
    if (!*one || strcmp(one, "nt") == 0) {
        return_with_nt();
    }
    printf("kernelentry: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
