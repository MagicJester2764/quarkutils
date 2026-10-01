#define _GNU_SOURCE /* F_OFD_* */
/* Record locks and flock, within one program. dtest checks them across two. */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/file.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

#define F "/tmp/locktest"

static int ofd(int fd, int cmd, short type, off_t start, off_t len) {
    struct flock fl = { .l_type = type, .l_whence = SEEK_SET, .l_start = start, .l_len = len };
    return fcntl(fd, cmd, &fl);
}

static int holder;

static void *release_later(void *arg) {
    (void)arg;
    struct timespec ts = { 0, 200 * 1000 * 1000 };
    nanosleep(&ts, NULL);
    ofd(holder, F_OFD_SETLK, F_UNLCK, 0, 0);
    return NULL;
}

static long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

int main(void) {
    printf("locks:\n");
    int a = open(F, O_RDWR | O_CREAT | O_TRUNC, 0644);
    int b = open(F, O_RDWR);
    write(a, "0123456789abcdefghij", 20);
    struct flock q = { .l_type = F_WRLCK, .l_whence = SEEK_SET };
    check("a program's own lock", ofd(a, F_SETLK, F_WRLCK, 0, 0) == 0);
    check("does not conflict with itself", fcntl(b, F_GETLK, &q) == 0 && q.l_type == F_UNLCK);
    check("and goes when any of its descriptors closes",
          ofd(a, F_SETLK, F_UNLCK, 0, 0) == 0);
    check("an open file's lock", ofd(a, F_OFD_SETLK, F_WRLCK, 0, 10) == 0);
    errno = 0;
    check("conflicts with another open file", ofd(b, F_OFD_SETLK, F_WRLCK, 5, 10) == -1 &&
          (errno == EAGAIN || errno == EACCES));
    check("but not beside it", ofd(b, F_OFD_SETLK, F_WRLCK, 10, 10) == 0);
    struct flock g = { .l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 5 };
    check("F_OFD_GETLK names the holder's range", fcntl(b, F_OFD_GETLK, &g) == 0 &&
          g.l_type == F_WRLCK && g.l_start == 0 && g.l_len == 10 && g.l_pid == -1);
    ofd(a, F_OFD_SETLK, F_UNLCK, 0, 0);
    ofd(b, F_OFD_SETLK, F_UNLCK, 0, 0);
    check("two shared locks", ofd(a, F_OFD_SETLK, F_RDLCK, 0, 0) == 0 &&
          ofd(b, F_OFD_SETLK, F_RDLCK, 0, 0) == 0);
    check("keep out an exclusive one", ofd(b, F_OFD_SETLK, F_WRLCK, 0, 0) == -1);
    ofd(a, F_OFD_SETLK, F_UNLCK, 0, 0);
    ofd(b, F_OFD_SETLK, F_UNLCK, 0, 0);
    holder = a;
    ofd(a, F_OFD_SETLK, F_WRLCK, 0, 0);
    pthread_t t;
    pthread_create(&t, NULL, release_later, NULL);
    long before = now_ms();
    int waited = ofd(b, F_OFD_SETLKW, F_WRLCK, 0, 0) == 0;
    long took = now_ms() - before;
    pthread_join(t, NULL);
    check("F_OFD_SETLKW waits for the holder", waited && took >= 100);
    ofd(b, F_OFD_SETLK, F_UNLCK, 0, 0);
    check("flock takes the whole file", flock(a, LOCK_EX) == 0);
    errno = 0;
    check("and keeps out another open file", flock(b, LOCK_EX | LOCK_NB) == -1 && errno == EWOULDBLOCK);
    check("until it is unlocked", flock(a, LOCK_UN) == 0 && flock(b, LOCK_EX | LOCK_NB) == 0);
    flock(b, LOCK_UN);
    ofd(a, F_OFD_SETLK, F_WRLCK, 0, 0);
    close(a);
    check("closing a descriptor drops its lock", ofd(b, F_OFD_SETLK, F_WRLCK, 0, 0) == 0);
    close(b);
    check("tidy up", unlink(F) == 0);
    printf("locktest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
