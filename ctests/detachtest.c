/* A detached thread: one nobody joins. Its stack is its own to give back,
 * which it does as the last thing it does — standing on it.
 *
 * musl ends such a thread with two system calls and nothing between them.
 * That file of musl made them straight at the kernel, in Linux's numbers,
 * and the first detached thread to end took its program with it. */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static volatile int ran;

static void *work(void *arg) {
    __sync_fetch_and_add(&ran, 1);
    return arg;
}

static void *slow(void *arg) {
    usleep(100 * 1000);
    __sync_fetch_and_add(&ran, 1);
    return arg;
}

int main(void) {
    printf("detached threads:\n");
    pthread_attr_t detached;
    pthread_t t;
    void *said = 0;

    pthread_attr_init(&detached);
    pthread_attr_setdetachstate(&detached, PTHREAD_CREATE_DETACHED);
    check("a thread is started detached", pthread_create(&t, &detached, work, 0) == 0);
    usleep(200 * 1000);
    check("it ran, and its ending was not the program's", ran == 1);

    /* One after another: each ends on the same borrowed ground. */
    int started = 0;
    for (int i = 0; i < 20; i++) {
        started += pthread_create(&t, &detached, work, 0) == 0;
        usleep(20 * 1000);
    }
    usleep(100 * 1000);
    check("twenty more came and went", started == 20 && ran == 21);

    /* Several at once, ending as they please. */
    started = 0;
    for (int i = 0; i < 8; i++) {
        started += pthread_create(&t, &detached, slow, 0) == 0;
    }
    usleep(400 * 1000);
    check("eight at once", started == 8 && ran == 29);

    check("a thread detached after it was started",
          pthread_create(&t, 0, slow, 0) == 0 && pthread_detach(t) == 0);
    usleep(300 * 1000);
    check("ends the same way", ran == 30);

    check("and a thread that is joined still is",
          pthread_create(&t, 0, work, (void *)5) == 0 && pthread_join(t, &said) == 0 &&
              said == (void *)5);

    printf("detachtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
