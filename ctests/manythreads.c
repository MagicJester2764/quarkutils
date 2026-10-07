/* A thousand threads of one program at once, each alive until all of them
 * are, and then joined. Sixty-four tasks were the machine's, and sixteen a
 * program's: pthread_create refused the seventeenth. They wait by sleeping,
 * as a futex has room for sixty-four waiters. */
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>

#define N 1000

static atomic_int here;
static atomic_int go;

static void nap(long ms) {
    struct timespec t = { ms / 1000, (ms % 1000) * 1000000 };
    nanosleep(&t, 0);
}

static void *member(void *arg) {
    atomic_fetch_add(&here, 1);
    while (!atomic_load(&go)) {
        nap(20);
    }
    return arg;
}

int main(void) {
    static pthread_t t[N];
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 16384);
    int made = 0;
    while (made < N && pthread_create(&t[made], &attr, member, (void *)(long)made) == 0) {
        made++;
    }
    for (int waited = 0; atomic_load(&here) < made && waited < 1000; waited++) {
        nap(10);
    }
    int together = atomic_load(&here);
    atomic_store(&go, 1);
    int joined = 0;
    for (int i = 0; i < made; i++) {
        void *back = 0;
        if (pthread_join(t[i], &back) == 0 && back == (void *)(long)i) {
            joined++;
        }
    }
    if (made != N || together != N || joined != N) {
        printf("manythreads: FAILED (%d made, %d at once, %d joined)\n", made, together, joined);
        return 1;
    }
    printf("manythreads: ok\n");
    return 0;
}
