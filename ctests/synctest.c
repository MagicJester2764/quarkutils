/* fsync, fdatasync and sync: a write is answered before the filesystem has
 * recorded it for good, and these wait until it has.
 *
 * What can be checked from inside is that they succeed, refuse what is not
 * open, and leave the file as it was written. That they do what they say —
 * that a machine stopped straight after one still has the file — is checked
 * from outside: `synctest leave PATH` writes a file of a known pattern,
 * syncs and says so, and whoever is watching stops the machine and looks at
 * the disk. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define PAGES 75

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* Page `n` of the pattern: every byte says which page and where. */
static void pattern(unsigned char *page, int n) {
    for (int i = 0; i < 4096; i++) {
        page[i] = (unsigned char)(n * 131 + i * 7 + 3);
    }
}

static int write_pattern(const char *path) {
    unsigned char page[4096];
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) {
        return -1;
    }
    for (int n = 0; n < PAGES; n++) {
        pattern(page, n);
        if (write(fd, page, sizeof page) != (ssize_t)sizeof page) {
            close(fd);
            return -1;
        }
    }
    return fd;
}

int main(int argc, char **argv) {
    if (argc == 3 && !strcmp(argv[1], "leave")) {
        int fd = write_pattern(argv[2]);
        if (fd < 0 || fsync(fd) != 0) {
            printf("synctest: could not leave %s\n", argv[2]);
            return 1;
        }
        printf("synctest: left %s, %d pages, synced\n", argv[2], PAGES);
        return 0;
    }

    printf("sync:\n");
    const char *path = "/tmp/synctest.file";
    int fd = write_pattern(path);
    check("a file is written, pages of it", fd >= 0);
    check("fsync waits for it and succeeds", fsync(fd) == 0);
    check("so does fdatasync", fdatasync(fd) == 0);
    sync();
    check("and sync, which has no answer to give", 1);
    check("syncfs is the same for the filesystem a file is in", syncfs(fd) == 0);
    close(fd);

    unsigned char want[4096], got[4096];
    int same = 1;
    fd = open(path, O_RDONLY);
    for (int n = 0; n < PAGES && same; n++) {
        pattern(want, n);
        same = read(fd, got, sizeof got) == (ssize_t)sizeof got && !memcmp(want, got, sizeof got);
    }
    check("the file is what was written", fd >= 0 && same && read(fd, got, 1) == 0);
    close(fd);

    errno = 0;
    check("a descriptor that is not open is refused", fsync(fd) == -1 && errno == EBADF);
    errno = 0;
    check("and so is one that never was", fdatasync(4000) == -1 && errno == EBADF);
    int p[2];
    errno = 0;
    check("a pipe is not something that is recorded",
          pipe(p) == 0 && fsync(p[1]) == -1 && errno == EINVAL);
    close(p[0]);
    close(p[1]);
    unlink(path);

    printf("synctest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
