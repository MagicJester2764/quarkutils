/* A signal for a program goes to a thread that lets it through, whichever
 * of its threads that is: of twenty, all but the last hold SIGUSR1 back, and
 * the last runs the handler. It is asleep three seconds at a time, so it runs
 * it within the second only if the kernel ends its sleep for it. A program's
 * tasks were read sixteen at a time, and the last of twenty-one was not
 * among them: nobody was woken, and the signal waited for the sleep. */
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>
#include <unistd.h>

#define N 20

static pthread_t t[N];
static atomic_int ready;
static atomic_int go;
static atomic_int by_last;
static atomic_int by_other;

static void nap(long ms) {
    struct timespec s = { ms / 1000, (ms % 1000) * 1000000 };
    nanosleep(&s, 0);
}

static void on_usr1(int sig) {
    (void)sig;
    if (pthread_equal(pthread_self(), t[N - 1])) {
        atomic_fetch_add(&by_last, 1);
    } else {
        atomic_fetch_add(&by_other, 1);
    }
}

static void *member(void *arg) {
    /* Each begins with its maker's mask, SIGUSR1 held back; the last lets
     * it through. */
    int last = (long)arg == N - 1;
    if (last) {
        sigset_t usr1;
        sigemptyset(&usr1);
        sigaddset(&usr1, SIGUSR1);
        pthread_sigmask(SIG_UNBLOCK, &usr1, 0);
    }
    atomic_fetch_add(&ready, 1);
    while (!atomic_load(&go)) {
        nap(last ? 3000 : 5);
    }
    return 0;
}

int main(void) {
    struct sigaction sa = { 0 };
    sa.sa_handler = on_usr1;
    sigaction(SIGUSR1, &sa, 0);
    sigset_t usr1;
    sigemptyset(&usr1);
    sigaddset(&usr1, SIGUSR1);
    pthread_sigmask(SIG_BLOCK, &usr1, 0);
    int made = 0;
    while (made < N && pthread_create(&t[made], 0, member, (void *)(long)made) == 0) {
        made++;
    }
    for (int waited = 0; atomic_load(&ready) < made && waited < 500; waited++) {
        nap(10);
    }
    /* Handled within the second, or not: what the count is then. */
    int in_time = 0;
    if (made == N && atomic_load(&ready) == N) {
        kill(getpid(), SIGUSR1);
        for (int waited = 0; waited < 100 && !atomic_load(&by_last) && !atomic_load(&by_other); waited++) {
            nap(10);
        }
        in_time = atomic_load(&by_last);
    }
    atomic_store(&go, 1);
    for (int i = 0; i < made; i++) {
        pthread_join(t[i], 0);
    }
    if (made != N || in_time != 1 || atomic_load(&by_last) != 1 || atomic_load(&by_other) != 0) {
        printf("sigthreads: FAILED (%d threads; within the second the last handled it %d times, and in all %d; others %d)\n",
               made, in_time, atomic_load(&by_last), atomic_load(&by_other));
        return 1;
    }
    printf("sigthreads: ok\n");
    return 0;
}
