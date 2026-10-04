// LINK: -dynamic -L@OUT@ -ldltwo
/* A program that loads a shared library.
 *
 * It is linked to the C library as a shared object, and to `libdltwo.so`,
 * which the dynamic loader finds and loads before it runs; and it opens
 * `libdlone.so` itself, by its path and then by its name, and finds what is
 * in it. The C library is one copy, shared by the program and both
 * libraries: errno set in a library is the program's errno, a thread-local
 * of a library opened later is every thread's own, and a constructor runs
 * when its library is opened, and a signal's handler runs — which needs a
 * constructor of the shared C library's own to have run.
 *
 * Exits 0 only if every check holds.
 */
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <sys/auxv.h>

extern int dltwo_calls;
int dltwo_twice(int x);

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static int (*tls_get)(void);
static volatile sig_atomic_t caught;

static void handler(int sig)
{
    caught = sig;
}

static void *other_thread(void *arg)
{
    (void)arg;
    return (void *)(long)tls_get();
}

int main(void)
{
    printf("dltest:\n");
    check("it was started by a dynamic loader", getauxval(AT_BASE) != 0);
    /* Where the kernel enters a program to run a handler is said by a
       constructor of the C library's own, which is a shared object here. */
    signal(SIGUSR1, handler);
    raise(SIGUSR1);
    check("a signal's handler runs: the shared C library's constructor ran", caught == SIGUSR1);
    check("a library it is linked to was loaded before it ran, and is called",
          dltwo_twice(21) == 42 && dltwo_calls == 1);

    void *h = dlopen("/usr/lib/libdlone.so", RTLD_NOW);
    check("a library is opened by its path", h != NULL);
    if (!h) {
        printf("  (%s)\n", dlerror());
        printf("dltest: FAILED\n");
        return 1;
    }
    int (*add)(int, int) = (int (*)(int, int))dlsym(h, "dlone_add");
    check("a function in it is found, and called", add && add(2, 3) == 5);
    int *value = dlsym(h, "dlone_value");
    check("and a datum", value && *value == 1234);
    int *made = dlsym(h, "dlone_constructed");
    check("its constructor ran when it was opened", made && *made == 42);
    check("a name it does not have is not found, and the loader says why",
          dlsym(h, "dlone_nothing") == NULL && dlerror() != NULL);

    int (*err)(void) = (int (*)(void))dlsym(h, "dlone_errno");
    check("errno it sets is this program's: one C library", err && err() == EBADF && errno == EBADF);

    tls_get = (int (*)(void))dlsym(h, "dlone_tls_get");
    void (*tls_set)(int) = (void (*)(int))dlsym(h, "dlone_tls_set");
    void *theirs = NULL;
    pthread_t t;
    int ran = 0;
    if (tls_get && tls_set) {
        tls_set(9);
        ran = pthread_create(&t, NULL, other_thread, NULL) == 0 && pthread_join(t, &theirs) == 0;
    }
    check("its thread-local is each thread's own",
          ran && tls_get() == 9 && (long)theirs == 7);

    check("it is closed", dlclose(h) == 0);
    void *again = dlopen("libdlone.so", RTLD_NOW);
    check("and opened again by its name, from where libraries are", again != NULL);
    if (again)
        dlclose(again);

    printf("dltest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
