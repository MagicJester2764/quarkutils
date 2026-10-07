/* Files, directories and pipes at descriptor 64 and above. A program's
 * descriptors go past 63 now, and what its C library keeps about each one —
 * which are files and their handles, which must not wait — has to go as far:
 * it was kept in arrays and words of sixty-four. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    int last = -1;
    int n = 0;
    while (n < 100 && (last = open("/etc/passwd", O_RDONLY)) >= 0) {
        n++;
    }
    if (n != 100 || last < 64) {
        printf("fdfiles: FAILED (%d opened, the last at %d)\n", n, last);
        return 1;
    }
    struct stat st;
    if (fstat(last, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size <= 0) {
        printf("fdfiles: FAILED (fstat of %d: errno %d)\n", last, errno);
        return 1;
    }
    char got[4];
    if (pread(last, got, 4, 0) != 4 || memcmp(got, "root", 4) != 0) {
        printf("fdfiles: FAILED (pread of %d)\n", last);
        return 1;
    }
    if (lseek(last, 0, SEEK_END) != st.st_size) {
        printf("fdfiles: FAILED (lseek of %d)\n", last);
        return 1;
    }
    int dir = open("/etc", O_RDONLY | O_DIRECTORY);
    int rel = dir >= 0 ? openat(dir, "passwd", O_RDONLY) : -1;
    if (dir < 64 || rel < 0) {
        printf("fdfiles: FAILED (openat from %d: errno %d)\n", dir, errno);
        return 1;
    }
    /* A mark the library keeps: an empty pipe asked not to wait answers
       at once. Kept nowhere, the read waits, and the alarm ends it. */
    int p[2];
    if (pipe(p) != 0 || p[0] < 64 || fcntl(p[0], F_SETFL, O_NONBLOCK) != 0) {
        printf("fdfiles: FAILED (a pipe at %d made non-blocking)\n", p[0]);
        return 1;
    }
    alarm(5);
    char c;
    errno = 0;
    if (read(p[0], &c, 1) != -1 || errno != EAGAIN) {
        printf("fdfiles: FAILED (a read of %d that should not wait: errno %d)\n", p[0], errno);
        return 1;
    }
    alarm(0);
    printf("fdfiles: ok\n");
    return 0;
}
