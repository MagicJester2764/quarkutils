/* Reading directories, and the calls realpath and df lean on. */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/utsname.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

#define TESTDIR "/tmp/dirtest"
#define COUNT 100
/* Long enough that a page of them needs more than one read. */
#define NAME "%s/entry-%03d-with-a-name-long-enough-to-need-more-than-one-page"

static int list(DIR *d, char seen[COUNT], int *dots, int *types_ok) {
    struct dirent *e;
    int n = 0;
    memset(seen, 0, COUNT);
    *dots = 0;
    *types_ok = 1;
    while ((e = readdir(d))) {
        int i;
        if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) {
            (*dots)++;
            *types_ok &= e->d_type == DT_DIR;
        } else if (sscanf(e->d_name, "entry-%03d-", &i) == 1 && i >= 0 && i < COUNT) {
            seen[i]++;
            *types_ok &= e->d_type == DT_REG;
            n++;
        }
    }
    return n;
}

int main(void) {
    char path[256], seen[COUNT];
    int dots, types_ok;
    printf("directories:\n");
    mkdir(TESTDIR, 0755);
    for (int i = 0; i < COUNT; i++) {
        snprintf(path, sizeof path, NAME, TESTDIR, i);
        int fd = open(path, O_WRONLY | O_CREAT, 0644);
        if (fd >= 0) {
            close(fd);
        }
    }
    DIR *d = opendir(TESTDIR);
    check("open a directory", d != NULL);
    int n = d ? list(d, seen, &dots, &types_ok) : 0;
    int once = 1;
    for (int i = 0; i < COUNT; i++) {
        once &= seen[i] == 1;
    }
    check("every entry, once, across pages", n == COUNT && once);
    check("with . and ..", dots == 2);
    check("and the right types", types_ok);
    if (d) {
        rewinddir(d);
        check("again after rewinding", list(d, seen, &dots, &types_ok) == COUNT);
        closedir(d);
    }
    errno = 0;
    check("a file is not a directory", opendir("/etc/passwd") == NULL && errno == ENOTDIR);

    struct statfs fs;
    check("statfs says ext2", statfs("/", &fs) == 0 && fs.f_type == 0xEF53 && fs.f_blocks > 0 && fs.f_bfree <= fs.f_blocks);
    char link[64];
    check("readlink of a file is EINVAL", readlink("/etc/passwd", link, sizeof link) == -1 && errno == EINVAL);
    check("readlink of nothing is ENOENT", readlink("/no/such", link, sizeof link) == -1 && errno == ENOENT);
    char real[PATH_MAX];
    check("realpath resolves", realpath("/etc/../etc/passwd", real) && !strcmp(real, "/etc/passwd"));
    check("the working directory is where chdir put it",
          chdir("/") == 0 && getcwd(real, sizeof real) && !strcmp(real, "/"));
    struct utsname u;
    check("uname says Quark", uname(&u) == 0 && !strcmp(u.sysname, "Quark") && !strcmp(u.machine, "x86_64"));

    int gone = 1;
    for (int i = 0; i < COUNT; i++) {
        snprintf(path, sizeof path, NAME, TESTDIR, i);
        gone &= unlink(path) == 0;
    }
    check("tidy up", gone && rmdir(TESTDIR) == 0);
    printf("dirtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
