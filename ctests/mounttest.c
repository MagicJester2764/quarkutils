/* A filesystem mounted in another, as a program written for Unix sees it:
 * a device of its own under `stat`, a rename that will not cross, a working
 * directory inside it, and what `statfs` and `/etc/mtab` say.
 *
 * The filesystem is made with mke2fs on a disk of memory and mounted with
 * the system's own `mount`; a system with neither has nothing to test. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <mntent.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/wait.h>
#include <unistd.h>

#define BLKGETSIZE64 0x80081272
#define SIZE (32L << 20)
#define AT "/tmp/mounttest"

extern char **environ;

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static int run(char *const argv[]) {
    char path[64];
    posix_spawn_file_actions_t actions;
    pid_t pid;
    int status;

    snprintf(path, sizeof path, "/usr/bin/%s", argv[0]);
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0);
    int err = posix_spawn(&pid, path, &actions, NULL, argv, environ);
    posix_spawn_file_actions_destroy(&actions);
    if (err || waitpid(pid, &status, 0) != pid || !WIFEXITED(status)) {
        return -1;
    }
    return WEXITSTATUS(status);
}

#define RUN(...) run((char *const[]){ __VA_ARGS__, NULL })

static int find(char *path, size_t len) {
    for (int tries = 0; tries < 100; tries++) {
        for (int n = 0; n < 8; n++) {
            unsigned long long bytes = 0;
            snprintf(path, len, "/dev/ram%d", n);
            int fd = open(path, O_RDONLY);
            if (fd < 0) {
                continue;
            }
            int is = ioctl(fd, BLKGETSIZE64, &bytes) == 0 && bytes == SIZE;
            close(fd);
            if (is) {
                return 1;
            }
        }
        usleep(20 * 1000);
    }
    return 0;
}

/* Whether /etc/mtab has `dev` mounted on AT. */
static int in_mtab(const char *dev) {
    FILE *f = setmntent("/etc/mtab", "r");
    struct mntent *m;
    int found = 0;
    while (f && (m = getmntent(f))) {
        if (!strcmp(m->mnt_fsname, dev) && !strcmp(m->mnt_dir, AT) && !strcmp(m->mnt_type, "ext4")) {
            found = 1;
        }
    }
    if (f) {
        endmntent(f);
    }
    return found;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("a mounted filesystem:\n");
    if (access("/usr/bin/mkfs.ext4", X_OK) || access("/usr/bin/mount", X_OK)) {
        printf("  (nothing here to make a filesystem with, or to mount one)\n");
        return 0;
    }
    char dev[32];
    pid_t disk = 0;
    char *ramdisk[] = { "ramdisk", "32", NULL };
    if (posix_spawn(&disk, "/usr/bin/ramdisk", NULL, NULL, ramdisk, environ) || !find(dev, sizeof dev)) {
        check("a disk of memory to work on", 0);
        return 1;
    }
    RUN("umount", AT);
    mkdir(AT, 0755);
    struct stat around, on, st;
    check("a filesystem is made and mounted",
          RUN("mkfs.ext4", "-q", "-F", dev) == 0 && stat(AT, &around) == 0 && RUN("mount", dev, AT) == 0);
    check("/etc/mtab says so", in_mtab(dev));
    check("its root is on another device than the directory around it",
          stat(AT, &on) == 0 && stat("/tmp", &st) == 0 && on.st_dev != st.st_dev && around.st_dev == st.st_dev);
    check("and is inode 2 there, as an ext filesystem's root is", on.st_ino == 2);

    int fd = open(AT "/file", O_CREAT | O_RDWR | O_TRUNC, 0640);
    char text[64] = "";
    check("a file is made in it, written and read back",
          fd >= 0 && write(fd, "mounted\n", 8) == 8 && pread(fd, text, sizeof text, 0) == 8 &&
              !memcmp(text, "mounted\n", 8));
    check("fstat says it is there, with the mode asked for",
          fstat(fd, &st) == 0 && st.st_dev == on.st_dev && st.st_size == 8 && (st.st_mode & 0777) == 0640);
    check("it is lengthened and cut", ftruncate(fd, 4096) == 0 && lseek(fd, 0, SEEK_END) == 4096 &&
                                          ftruncate(fd, 3) == 0 && fstat(fd, &st) == 0 && st.st_size == 3);
    check("its owner and its mode are changed",
          fchmod(fd, 0600) == 0 && chown(AT "/file", 7, 8) == 0 && stat(AT "/file", &st) == 0 &&
              (st.st_mode & 0777) == 0600 && st.st_uid == 7 && st.st_gid == 8);

    struct statfs fs, by_path, root;
    check("statfs is of the filesystem the file is in, by descriptor and by path",
          fstatfs(fd, &fs) == 0 && statfs(AT, &by_path) == 0 && statfs("/", &root) == 0 &&
              fs.f_type == 0xEF53 && fs.f_blocks * fs.f_bsize > 24L << 20 && fs.f_blocks * fs.f_bsize <= SIZE &&
              by_path.f_blocks == fs.f_blocks && fs.f_blocks != root.f_blocks);
    close(fd);

    errno = 0;
    check("a rename out of it is refused as crossing devices",
          rename(AT "/file", "/tmp/mounttest.out") == -1 && errno == EXDEV);
    errno = 0;
    check("and so is a link across", link(AT "/file", "/tmp/mounttest.out") == -1 && errno == EXDEV);
    check("a rename inside it is not", mkdir(AT "/dir", 0755) == 0 && rename(AT "/file", AT "/dir/file") == 0);

    /* A directory's entries say the inode numbers stat does. */
    DIR *d = opendir(AT "/dir");
    struct dirent *e;
    int matched = 0;
    while (d && (e = readdir(d))) {
        if (!strcmp(e->d_name, "file")) {
            matched = stat(AT "/dir/file", &st) == 0 && e->d_ino == st.st_ino && e->d_type == DT_REG;
        }
    }
    if (d) {
        closedir(d);
    }
    check("a directory there lists its entries by the numbers stat gives", matched);

    char cwd[128];
    check("a program moves into it, and getcwd says where",
          chdir(AT "/dir") == 0 && getcwd(cwd, sizeof cwd) && !strcmp(cwd, AT "/dir"));
    check("a name is looked up from there", access("file", R_OK) == 0 && access("../dir/file", R_OK) == 0);
    int dirfd = open(".", O_RDONLY | O_DIRECTORY);
    check("and from a directory opened there",
          dirfd >= 0 && fstatat(dirfd, "file", &st, 0) == 0 && st.st_size == 3 &&
              openat(dirfd, "made", O_CREAT | O_WRONLY, 0644) >= 0);
    check("`..` from its root is the directory the mount is in",
          chdir(AT) == 0 && chdir("..") == 0 && getcwd(cwd, sizeof cwd) && !strcmp(cwd, "/tmp"));
    check("fchdir goes back in", fchdir(dirfd) == 0 && getcwd(cwd, sizeof cwd) && !strcmp(cwd, AT "/dir"));

    check("it is not unmounted while this is in it", RUN("umount", AT) == 1);
    chdir("/");
    check("or while a directory of it is open", RUN("umount", AT) == 1);
    /* Every descriptor: the one openat made as well. */
    for (int i = 3; i < 16; i++) {
        close(i);
    }
    check("it is unmounted when nothing is", RUN("umount", AT) == 0);
    check("and /etc/mtab no longer has it", !in_mtab(dev));
    check("the directory is on the root's device again", stat(AT, &st) == 0 && st.st_dev == around.st_dev);

    kill(disk, SIGKILL);
    waitpid(disk, NULL, 0);
    rmdir(AT);
    printf("mounttest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
