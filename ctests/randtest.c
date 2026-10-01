/* Random bytes from the kernel, and the devices every C program expects. */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/random.h>
#include <sys/stat.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* A page of random bytes uses most values, and none of them much. */
static int spread(const unsigned char *p, size_t n) {
    int seen[256] = {0}, distinct = 0, most = 0;
    for (size_t i = 0; i < n; i++) {
        seen[p[i]]++;
    }
    for (int i = 0; i < 256; i++) {
        distinct += seen[i] != 0;
        most = seen[i] > most ? seen[i] : most;
    }
    return distinct > 200 && most < 64;
}

int main(void) {
    unsigned char a[64], b[64], page[4096];
    printf("random:\n");
    check("getrandom fills a buffer", getrandom(a, sizeof a, 0) == sizeof a);
    check("and again, differently", getrandom(b, sizeof b, 0) == sizeof b && memcmp(a, b, sizeof a));
    int fd = open("/dev/urandom", O_RDONLY);
    check("/dev/urandom opens", fd >= 0);
    check("and reads a page that looks random",
          fd >= 0 && read(fd, page, sizeof page) == (ssize_t)sizeof page && spread(page, sizeof page));
    close(fd);
    struct stat st;
    check("/dev/null is a character device", stat("/dev/null", &st) == 0 && S_ISCHR(st.st_mode));
    fd = open("/dev/null", O_RDWR);
    check("/dev/null swallows writes", fd >= 0 && write(fd, "abc", 3) == 3);
    check("and reads as empty", fd >= 0 && read(fd, page, 16) == 0);
    close(fd);
    fd = open("/dev/zero", O_RDONLY);
    memset(page, 1, 32);
    int zeros = fd >= 0 && read(fd, page, 32) == 32;
    for (int i = 0; i < 32 && zeros; i++) {
        zeros = page[i] == 0;
    }
    check("/dev/zero reads zeroes", zeros);
    close(fd);
    fd = open("/dev/full", O_WRONLY);
    errno = 0;
    check("/dev/full is always full", fd >= 0 && write(fd, "x", 1) == -1 && errno == ENOSPC);
    close(fd);
    DIR *d = opendir("/dev");
    int names = 0;
    struct dirent *e;
    while (d && (e = readdir(d))) {
        names += !strcmp(e->d_name, "null") + !strcmp(e->d_name, "zero") +
                 !strcmp(e->d_name, "full") + !strcmp(e->d_name, "random") +
                 !strcmp(e->d_name, "urandom");
    }
    if (d) {
        closedir(d);
    }
    check("/dev lists all five", names == 5);
    printf("randtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
