// LINK: -dynamic -fPIE -pie
/* A program linked to be put anywhere — a PIE, as a program built for Linux
 * usually is — run where its loader put it.
 *
 * The loaders put such a program a random number of pages into a terabyte
 * of its own (QUARK_PIE_BASE), and tell its dynamic loader where: the copy
 * of its headers says so, and so does its entry. Everything that depends on
 * that is checked here — that it was moved at all, that what was linked to
 * name an address names the moved one (a table of functions, a pointer to a
 * string), that its thread-locals are its threads', that dl_iterate_phdr
 * says where it is, and that a fork of it is a copy that runs.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <link.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/wait.h>
#include <unistd.h>

#include <quark/layout.h>

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static int twice(int x) { return 2 * x; }
static int thrice(int x) { return 3 * x; }
static int (*const table[])(int) = { twice, thrice };
static const char *greeting = "moved";
static __thread int mine = 7;

static void *thread_main(void *arg)
{
    (void)arg;
    mine += 1;
    return (void *)(long)mine;
}

static unsigned long found_base;
static int found_main;

static int each(struct dl_phdr_info *info, size_t size, void *data)
{
    (void)size;
    (void)data;
    unsigned long at = (unsigned long)&each;
    for (int i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
        if (ph->p_type != PT_LOAD)
            continue;
        unsigned long from = info->dlpi_addr + ph->p_vaddr;
        if (at >= from && at < from + ph->p_memsz) {
            found_base = info->dlpi_addr;
            found_main = 1;
            return 1;
        }
    }
    return 0;
}

int main(void)
{
    unsigned long here = (unsigned long)&main;
    printf("pietest: main is at %#lx\n", here);
    check("it was moved into the window a PIE is put in",
          here >= QUARK_PIE_BASE && here < QUARK_PIE_BASE + QUARK_PIE_PAGES * 4096UL + (1UL << 30));
    check("a table of functions names where they were put",
          table[0](21) == 42 && table[1](14) == 42);
    check("a pointer to a string names where it was put", strcmp(greeting, "moved") == 0);
    check("its loader was told where it is (AT_ENTRY)",
          getauxval(AT_ENTRY) >= QUARK_PIE_BASE && getauxval(AT_ENTRY) < here + (1UL << 30));
    check("it asked for Quark's loader, and is not told it is Linux's", getauxval(QUARK_AT_LINUX) == 0);

    dl_iterate_phdr(each, 0);
    check("dl_iterate_phdr finds it, moved", found_main && found_base != 0);

    errno = 0;
    check("errno is its own", open("/no/such/file", O_RDONLY) < 0 && errno == ENOENT);

    pthread_t t;
    void *got = 0;
    int made = pthread_create(&t, 0, thread_main, 0) == 0 && pthread_join(t, &got) == 0;
    check("a thread's thread-local is its own", made && (long)got == 8 && mine == 7);

    pid_t child = fork();
    if (child == 0)
        _exit(table[0](3) == 6 && strcmp(greeting, "moved") == 0 ? 0 : 1);
    int status = -1;
    check("a fork of it runs where it was",
          child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);

    printf("pietest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
