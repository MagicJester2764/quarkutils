/* A file one thread opened is the program's, not the thread's. */
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int fd;
static char got[8];
static int nread = -1;

static void *reader(void *arg) {
    (void)arg;
    nread = (int)pread(fd, got, 4, 0);
    return NULL;
}

int main(void) {
    fd = open("/etc/passwd", O_RDONLY);
    pthread_t t;
    int started = fd >= 0 && pthread_create(&t, NULL, reader, NULL) == 0;
    if (started) {
        pthread_join(t, NULL);
    }
    int ok = started && nread == 4 && !memcmp(got, "root", 4);
    printf("threadfile: %s\n", ok ? "ok" : "FAILED");
    return ok ? 0 : 1;
}
