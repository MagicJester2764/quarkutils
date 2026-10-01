/* Symbolic links: made, read, followed where they should be, and not where
   they should not. `linktest keep` leaves one fast and one slow link in
   /tmp/linktest-kept for e2fsck to look at. */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
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

#define T "/tmp/linktest"
#define LONG_TARGET T "/a-directory-name-long-enough/that-the-target-cannot-fit-in-sixty-bytes/file"

static void tidy(void) {
    unlink(T "/fast"); unlink(T "/slow"); unlink(T "/dangling"); unlink(T "/rel");
    unlink(T "/dirlink"); unlink(T "/loop-a"); unlink(T "/loop-b"); unlink(T "/renamed");
    unlink(T "/null");
    unlink(LONG_TARGET);
    rmdir(T "/a-directory-name-long-enough/that-the-target-cannot-fit-in-sixty-bytes");
    rmdir(T "/a-directory-name-long-enough");
    unlink(T "/sub/inner"); rmdir(T "/sub");
    unlink(T "/file"); rmdir(T);
}

int main(int argc, char **argv) {
    int keep = argc > 1 && !strcmp(argv[1], "keep");
    char buf[PATH_MAX];
    struct stat st;
    printf("symbolic links:\n");
    tidy();
    mkdir(T, 0755);
    int fd = open(T "/file", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) { write(fd, "target", 6); close(fd); }
    mkdir(T "/a-directory-name-long-enough", 0755);
    mkdir(T "/a-directory-name-long-enough/that-the-target-cannot-fit-in-sixty-bytes", 0755);
    fd = open(LONG_TARGET, O_WRONLY | O_CREAT, 0644);
    if (fd >= 0) { write(fd, "far", 3); close(fd); }
    mkdir(T "/sub", 0755);
    fd = open(T "/sub/inner", O_WRONLY | O_CREAT, 0644);
    if (fd >= 0) close(fd);

    check("a short link", symlink(T "/file", T "/fast") == 0);
    check("reads back", readlink(T "/fast", buf, sizeof buf) == (ssize_t)strlen(T "/file") &&
          !memcmp(buf, T "/file", strlen(T "/file")));
    check("a long link", symlink(LONG_TARGET, T "/slow") == 0);
    check("reads back too", readlink(T "/slow", buf, sizeof buf) == (ssize_t)strlen(LONG_TARGET) &&
          !memcmp(buf, LONG_TARGET, strlen(LONG_TARGET)));
    check("readlink stops at the buffer", readlink(T "/slow", buf, 4) == 4);
    check("stat follows", stat(T "/fast", &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 6);
    check("lstat does not", lstat(T "/fast", &st) == 0 && S_ISLNK(st.st_mode) &&
          st.st_size == (off_t)strlen(T "/file"));
    fd = open(T "/slow", O_RDONLY);
    check("open follows", fd >= 0 && read(fd, buf, 3) == 3 && !memcmp(buf, "far", 3));
    close(fd);
    check("a relative link", symlink("sub/inner", T "/rel") == 0 && stat(T "/rel", &st) == 0);
    check("a link to a directory", symlink("sub", T "/dirlink") == 0 &&
          stat(T "/dirlink/inner", &st) == 0);
    DIR *d = opendir(T "/dirlink");
    check("opens as that directory", d != NULL);
    if (d) closedir(d);
    check("a dangling link", symlink(T "/nowhere", T "/dangling") == 0);
    check("stat says it leads nowhere", stat(T "/dangling", &st) == -1 && errno == ENOENT);
    check("lstat still sees it", lstat(T "/dangling", &st) == 0 && S_ISLNK(st.st_mode));
    symlink("loop-b", T "/loop-a");
    symlink("loop-a", T "/loop-b");
    errno = 0;
    check("a loop is an error", open(T "/loop-a", O_RDONLY) == -1 && errno == ELOOP);
    errno = 0;
    check("O_NOFOLLOW refuses a link", open(T "/fast", O_RDONLY | O_NOFOLLOW) == -1 && errno == ELOOP);
    check("a link to a device reaches the device", symlink("/dev/null", T "/null") == 0 &&
          stat(T "/null", &st) == 0 && S_ISCHR(st.st_mode));
    check("a link cannot be made over a name", symlink("x", T "/file") == -1 && errno == EEXIST);
    check("readlink of a file is EINVAL", readlink(T "/file", buf, sizeof buf) == -1 && errno == EINVAL);
    int lnk = 0;
    d = opendir(T);
    struct dirent *e;
    while (d && (e = readdir(d))) {
        if (!strcmp(e->d_name, "fast")) lnk = e->d_type == DT_LNK;
    }
    if (d) closedir(d);
    check("a directory lists a link as a link", lnk);
    char *real = realpath(T "/dirlink/inner", NULL);
    check("realpath resolves through a link", real && !strcmp(real, T "/sub/inner"));
    free(real);
    check("rename moves the link", rename(T "/rel", T "/renamed") == 0 &&
          readlink(T "/renamed", buf, sizeof buf) == 9);
    check("unlink removes the link", unlink(T "/fast") == 0 && stat(T "/file", &st) == 0);
    if (keep) {
        mkdir("/tmp/linktest-kept", 0755);
        unlink("/tmp/linktest-kept/fast");
        unlink("/tmp/linktest-kept/slow");
        symlink("/etc/passwd", "/tmp/linktest-kept/fast");
        symlink("/tmp/linktest-kept/a-name-that-is-well-over-sixty-bytes-long-so-it-needs-a-block", "/tmp/linktest-kept/slow");
    } else {
        unlink("/tmp/linktest-kept/fast");
        unlink("/tmp/linktest-kept/slow");
        rmdir("/tmp/linktest-kept");
    }
    tidy();
    check("tidy up", lstat(T, &st) == -1);
    printf("linktest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
