/* A working directory: relative paths, *at calls, and inheritance. */
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

#define D "/tmp/cwdtest"

int main(void) {
    char buf[PATH_MAX];
    struct stat st;
    printf("working directory:\n");
    unlink(D "/sub/file"); unlink(D "/sub/renamed"); unlink(D "/rel"); rmdir(D "/sub");
    rmdir(D "/gone"); rmdir(D);
    check("chdir to /", chdir("/") == 0);
    check("getcwd says /", getcwd(buf, sizeof buf) && !strcmp(buf, "/"));
    check("mkdir by absolute path", mkdir(D, 0755) == 0);
    check("chdir into it", chdir(D) == 0 && getcwd(buf, sizeof buf) && !strcmp(buf, D));
    check("mkdir by relative path", mkdir("sub", 0755) == 0 && stat(D "/sub", &st) == 0);
    int fd = open("sub/file", O_WRONLY | O_CREAT, 0644);
    check("create by relative path", fd >= 0 && write(fd, "cwd", 3) == 3);
    close(fd);
    check("stat by relative path", stat("sub/file", &st) == 0 && st.st_size == 3);
    int dfd = open("sub", O_RDONLY | O_DIRECTORY);
    check("open a directory", dfd >= 0);
    fd = openat(dfd, "file", O_RDONLY);
    check("openat reads from it", fd >= 0 && read(fd, buf, 3) == 3 && !memcmp(buf, "cwd", 3));
    close(fd);
    check("renameat within it", renameat(dfd, "file", dfd, "renamed") == 0 &&
          fstatat(dfd, "renamed", &st, 0) == 0);
    check("chdir ..", chdir("..") == 0 && getcwd(buf, sizeof buf) && !strcmp(buf, "/tmp"));
    check("fchdir back", fchdir(dfd) == 0 && getcwd(buf, sizeof buf) && !strcmp(buf, D "/sub"));
    check("unlinkat by a relative directory", unlinkat(AT_FDCWD, "renamed", 0) == 0);
    close(dfd);
    check("a directory named by a file is refused", chdir(D "/nothing") == -1 && errno == ENOENT);
    check("getcwd too small is ERANGE", getcwd(buf, 3) == NULL && errno == ERANGE);
    check("into a directory that then goes", mkdir(D "/gone", 0755) == 0 && chdir(D "/gone") == 0 &&
          rmdir(D "/gone") == 0);
    errno = 0;
    check("getcwd says it has gone", getcwd(buf, sizeof buf) == NULL && errno == ENOENT);
    check("and chdir away still works", chdir("/") == 0);
    check("tidy up", rmdir(D "/sub") == 0 && rmdir(D) == 0);
    printf("cwdtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
