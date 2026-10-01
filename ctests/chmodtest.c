/* What a file's inode says of it, and who may change it.
 *
 * `cp -a` copies a file and then makes the copy *be* the original: its mode,
 * its owner, its times. Every one of those was something the file server
 * chose once and nobody could change — every file 0644, every directory 0755,
 * dated when it was made — so a preserved copy was not one, and `chmod +x`
 * did not exist.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <unistd.h>

#define F "/tmp/chmodtest.file"
#define D "/tmp/chmodtest.dir"

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static unsigned mode_of(const char *path) {
    struct stat st;
    return stat(path, &st) == 0 ? (unsigned)(st.st_mode & 07777) : 0xFFFFu;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("modes, owners and times:\n");
    mkdir("/tmp", 0777);
    unlink(F);
    rmdir(D);
    struct stat st;

    /* The mode asked for, less the umask. */
    mode_t old = umask(022);
    int fd = open(F, O_WRONLY | O_CREAT | O_EXCL, 0666);
    check("a file is made with the mode asked for, less the umask", fd >= 0 && mode_of(F) == 0644);
    check("and a directory", mkdir(D, 0777) == 0 && mode_of(D) == 0755);
    umask(027);
    unlink(F);
    close(fd);
    fd = open(F, O_WRONLY | O_CREAT | O_EXCL, 0666);
    check("a tighter umask takes more off", fd >= 0 && mode_of(F) == 0640);
    check("umask says what it was", umask(old) == 027);

    check("chmod", chmod(F, 0755) == 0 && mode_of(F) == 0755);
    check("fchmod, through the descriptor", fchmod(fd, 0600) == 0 && mode_of(F) == 0600);
    check("the type is not the caller's to change",
          chmod(F, 0644) == 0 && stat(F, &st) == 0 && S_ISREG(st.st_mode));
    check("the special bits are kept", chmod(D, 01777) == 0 && mode_of(D) == 01777);
    check("chmod of nothing says so", chmod("/tmp/chmodtest.absent", 0644) == -1 && errno == ENOENT);

    /* A file with no execute bit is not a program, even to user 0. */
    check("a file nobody may run is not executable", access(F, X_OK) == -1);
    check("and exec says so, rather than calling it not a program",
          execl(F, F, (char *)NULL) == -1 && errno == EACCES);
    check("until it is marked so", chmod(F, 0755) == 0 && access(F, X_OK) == 0);
    check("when exec says what it is: not a program", execl(F, F, (char *)NULL) == -1 && errno == ENOEXEC);

    /* Owner and group: this runs as user 0, who may give a file away. */
    check("chown", chown(F, 12, 34) == 0 && stat(F, &st) == 0 && st.st_uid == 12 && st.st_gid == 34);
    check("-1 leaves one of them alone",
          chown(F, (uid_t)-1, 56) == 0 && stat(F, &st) == 0 && st.st_uid == 12 && st.st_gid == 56);
    check("fchown", fchown(fd, 0, 0) == 0 && fstat(fd, &st) == 0 && st.st_uid == 0 && st.st_gid == 0);

    /* Times: set, left alone, and now. */
    struct timespec t[2] = {{1000000000, 0}, {1234567890, 0}};
    check("utimensat sets both times",
          utimensat(AT_FDCWD, F, t, 0) == 0 && stat(F, &st) == 0 &&
              st.st_atime == 1000000000 && st.st_mtime == 1234567890);
    t[0].tv_nsec = UTIME_OMIT;
    t[1].tv_sec = 1111111111;
    check("or one and not the other",
          utimensat(AT_FDCWD, F, t, 0) == 0 && stat(F, &st) == 0 &&
              st.st_atime == 1000000000 && st.st_mtime == 1111111111);
    check("futimens, through the descriptor, to now",
          futimens(fd, NULL) == 0 && fstat(fd, &st) == 0 && st.st_mtime > 1234567890);
    check("changing any of it moves the change time", st.st_ctime >= st.st_mtime);

    /* A link is followed, unless asked otherwise. */
    unlink("/tmp/chmodtest.link");
    check("chmod through a link changes what it names",
          symlink(F, "/tmp/chmodtest.link") == 0 && chmod("/tmp/chmodtest.link", 0640) == 0 &&
              mode_of(F) == 0640);
    t[0].tv_sec = 5;
    t[0].tv_nsec = 0;
    t[1].tv_sec = 6;
    check("a time set without following is the link's own",
          utimensat(AT_FDCWD, "/tmp/chmodtest.link", t, AT_SYMLINK_NOFOLLOW) == 0 &&
              lstat("/tmp/chmodtest.link", &st) == 0 && st.st_mtime == 6 &&
              stat(F, &st) == 0 && st.st_mtime != 6);

    close(fd);
    unlink("/tmp/chmodtest.link");
    unlink(F);
    rmdir(D);
    printf("chmodtest: %d failed\n", failed);
    return failed ? 1 : 0;
}
