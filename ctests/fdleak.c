/* Descriptors a program gets back.
 *
 * cargo on Quark said it was out of descriptors building a module that on
 * Linux never had more than 17 open at once. It was not: the kernel had
 * refused it a ninth pipe, which the C library can only call EMFILE. This
 * is what was looked at first, and is kept because a descriptor left
 * behind would look the same: each loop does one of the things cargo does,
 * many times, and checks that the lowest free descriptor is where it was.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static int failed;

/* The descriptor the next open would be given. */
static int lowest(void)
{
    int fd = dup(0);
    if (fd >= 0)
        close(fd);
    return fd;
}

static void check(const char *what, int base)
{
    int now = lowest();
    if (now == base) {
        printf("  ok    %s\n", what);
    } else {
        printf("  FAIL  %s (the lowest free descriptor was %d, and is %d)\n", what, base, now);
        failed = 1;
    }
}

static char self[256];

/* Start this program again as Rust's standard library starts one: its
   output and errors to pipes, a pair of messages to hear how the exec went,
   fork, exec. */
static void start_as_rust(void)
{
    int out[2], err[2], sv[2];
    if (pipe2(out, O_CLOEXEC) || pipe2(err, O_CLOEXEC) ||
        socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv))
        return;
    pid_t pid = fork();
    if (pid == 0) {
        close(sv[0]);
        dup2(out[1], 1);
        dup2(err[1], 2);
        execl(self, self, "child", (char *)0);
        int e = errno;
        write(sv[1], &e, sizeof e);
        _exit(127);
    }
    close(sv[1]);
    close(out[1]);
    close(err[1]);
    char b[64];
    read(sv[0], b, sizeof b);
    close(sv[0]);
    while (read(out[0], b, sizeof b) > 0)
        ;
    while (read(err[0], b, sizeof b) > 0)
        ;
    close(out[0]);
    close(err[0]);
    waitpid(pid, 0, 0);
}

static void *opener(void *unused)
{
    (void)unused;
    for (int i = 0; i < 50; i++) {
        int fd = open("/etc/passwd", O_RDONLY | O_CLOEXEC);
        if (fd >= 0)
            close(fd);
    }
    return 0;
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "child") == 0) {
        printf("out\n");
        fprintf(stderr, "err\n");
        return 0;
    }
    if (strchr(argv[0], '/'))
        snprintf(self, sizeof self, "%s", argv[0]);
    else
        snprintf(self, sizeof self, "/usr/bin/%s", argv[0]);

    int base = lowest();
    printf("fdleak: the lowest free descriptor is %d\n", base);

    for (int i = 0; i < 100; i++) {
        DIR *d = opendir("/usr/bin");
        if (!d)
            break;
        while (readdir(d))
            ;
        closedir(d);
    }
    check("a directory listed a hundred times", base);

    for (int i = 0; i < 100; i++) {
        DIR *d = opendir("/usr");
        if (!d)
            break;
        struct dirent *e;
        struct stat st;
        while ((e = readdir(d)))
            fstatat(dirfd(d), e->d_name, &st, AT_SYMLINK_NOFOLLOW);
        closedir(d);
    }
    check("and each name in one looked at from it, a hundred times", base);

    for (int i = 0; i < 100; i++) {
        struct stat st;
        stat("/etc/passwd", &st);
        lstat("/usr/bin", &st);
        access("/etc/passwd", R_OK);
        free(realpath("/usr/bin/../lib", 0));
        char cwd[256];
        getcwd(cwd, sizeof cwd);
    }
    check("stat, lstat, access, realpath and getcwd, a hundred times", base);

    for (int i = 0; i < 100; i++) {
        int fd = open("/etc/passwd", O_RDONLY | O_CLOEXEC);
        char b[16];
        read(fd, b, sizeof b);
        close(fd);
        fd = openat(AT_FDCWD, "/usr", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        int in = openat(fd, "bin", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        close(in);
        close(fd);
    }
    check("files and directories opened and closed, a hundred times", base);

    for (int i = 0; i < 100; i++) {
        int p[2], sv[2];
        pipe2(p, O_CLOEXEC);
        close(p[0]);
        close(p[1]);
        socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv);
        close(sv[0]);
        close(sv[1]);
        socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv);
        close(sv[0]);
        close(sv[1]);
        int fd = fcntl(0, F_DUPFD_CLOEXEC, 3);
        close(fd);
    }
    check("pipes, pairs and copies made and closed, a hundred times", base);

    for (int i = 0; i < 100; i++) {
        int fd = open("/tmp/fdleak-lock", O_RDWR | O_CREAT | O_CLOEXEC, 0644);
        flock(fd, LOCK_EX);
        flock(fd, LOCK_UN);
        close(fd);
        mkdir("/tmp/fdleak-dir", 0755);
        rename("/tmp/fdleak-dir", "/tmp/fdleak-dir2");
        rmdir("/tmp/fdleak-dir2");
    }
    unlink("/tmp/fdleak-lock");
    check("a file locked, a directory made, renamed and removed, a hundred times", base);

    pthread_t t[4];
    for (int i = 0; i < 4; i++)
        pthread_create(&t[i], 0, opener, 0);
    for (int i = 0; i < 4; i++)
        pthread_join(t[i], 0);
    check("four threads opening and closing a file fifty times each", base);

    for (int i = 0; i < 30; i++)
        start_as_rust();
    check("a program started as Rust's standard library starts one, thirty times", base);

    for (int i = 0; i < 30; i++) {
        int p[2];
        pipe2(p, O_CLOEXEC);
        posix_spawn_file_actions_t fa;
        posix_spawn_file_actions_init(&fa);
        posix_spawn_file_actions_adddup2(&fa, p[1], 1);
        posix_spawn_file_actions_adddup2(&fa, p[1], 2);
        char *args[] = { self, "child", 0 };
        pid_t pid;
        if (posix_spawn(&pid, self, &fa, 0, args, environ) == 0) {
            close(p[1]);
            char b[64];
            while (read(p[0], b, sizeof b) > 0)
                ;
            waitpid(pid, 0, 0);
        } else {
            close(p[1]);
        }
        close(p[0]);
        posix_spawn_file_actions_destroy(&fa);
    }
    check("a program started with posix_spawn, thirty times", base);

    printf("fdleak: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
