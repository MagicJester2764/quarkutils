/* What a shell does to the programs it runs.
 *
 * A shell is the first program here that expects a file to be an ordinary
 * descriptor: opened in one program, put where standard output was, and still
 * there in the program that one becomes. Every check below is something bash
 * does on the way to running `cmd > file`, `a | b`, `cd dir; cmd` or
 * `{ a; b; } > file`, and each failed before a file was a descriptor the
 * kernel counts:
 *
 *   - a file could not be `dup2`ed onto 0, 1 or 2;
 *   - what a program had open did not survive `exec`;
 *   - a forked child held numbers the file server would not answer for;
 *   - and the position was each program's own, so a child writing after its
 *     parent wrote over it.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define SELF "/usr/bin/redirtest"
#define OUT  "/tmp/redirtest.out"

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* Run this program again as `how`, with whatever 0, 1 and 2 are now, and
   return its exit status. */
static int run(const char *how, const char *arg) {
    pid_t pid = fork();
    if (pid == 0) {
        execl(SELF, SELF, how, arg, (char *)NULL);
        _exit(111);
    }
    int status = 0;
    if (pid < 0 || waitpid(pid, &status, 0) != pid || !WIFEXITED(status)) {
        return -1;
    }
    return WEXITSTATUS(status);
}

/* The whole of a file, as a string. */
static const char *slurp(const char *path) {
    static char buf[256];
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return "<cannot open>";
    }
    long n = read(fd, buf, sizeof buf - 1);
    close(fd);
    buf[n < 0 ? 0 : n] = 0;
    return buf;
}

/* What this program does when it is the child. Nothing here knows what its
   descriptors are: it writes to 1 and reads from 0. */
static int child(const char *how, const char *arg) {
    if (!strcmp(how, "say")) {
        return write(1, "said\n", 5) == 5 ? 0 : 1;
    }
    if (!strcmp(how, "count")) {
        char buf[64];
        long total = 0, n;
        while ((n = read(0, buf, sizeof buf)) > 0) {
            total += n;
        }
        return (int)total;
    }
    if (!strcmp(how, "copy")) {
        char buf[64];
        long n;
        while ((n = read(0, buf, sizeof buf)) > 0) {
            if (write(1, buf, (size_t)n) != n) {
                return 1;
            }
        }
        return 0;
    }
    if (!strcmp(how, "isopen")) {
        int fd = 0;
        for (const char *d = arg; *d; d++) {
            fd = fd * 10 + (*d - '0');
        }
        return fcntl(fd, F_GETFD) >= 0;
    }
    if (!strcmp(how, "where")) {
        char cwd[64];
        return getcwd(cwd, sizeof cwd) && !strcmp(cwd, arg) && access("passwd", F_OK) == 0 ? 0 : 1;
    }
    if (!strcmp(how, "make")) {
        int fd = open(arg, O_WRONLY | O_CREAT | O_TRUNC, 0666);
        return fd < 0 ? 1 : (close(fd), 0);
    }
    if (!strcmp(how, "sleep")) {
        usleep(300000);
        return 7;
    }
    return 100;
}

int main(int argc, char **argv) {
    if (argc > 1) {
        return child(argv[1], argc > 2 ? argv[2] : "");
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("what a shell does:\n");
    mkdir("/tmp", 0777);

    /* cmd > file: the child opens it, puts it on 1, and becomes the command. */
    pid_t pid = fork();
    if (pid == 0) {
        int fd = open(OUT, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd < 0 || dup2(fd, 1) != 1 || close(fd) != 0) {
            _exit(112);
        }
        execl(SELF, SELF, "say", (char *)NULL);
        _exit(111);
    }
    int status = -1;
    check("a child redirects its output and becomes a program",
          pid > 0 && waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    check("and the program wrote into the file", !strcmp(slurp(OUT), "said\n"));

    /* { a; b; } > file: the shell itself holds the file on 1 while two
       programs run. Each writes after the one before, because the position
       is the file's and not theirs. */
    int saved = dup(1);
    int fd = open(OUT, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    int on = fd >= 0 && saved >= 0 && dup2(fd, 1) == 1;
    close(fd);
    int a = on ? run("say", "") : -1;
    int b = on ? run("say", "") : -1;
    long own = on ? write(1, "shell\n", 6) : -1;
    dup2(saved, 1);
    close(saved);
    check("a file held on descriptor 1 across two programs", on && a == 0 && b == 0 && own == 6);
    check("each wrote after the last", !strcmp(slurp(OUT), "said\nsaid\nshell\n"));

    /* The same without an exec in the way: one open file, a parent and a
       child, three writes. */
    fd = open(OUT, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    write(fd, "a\n", 2);
    pid = fork();
    if (pid == 0) {
        _exit(write(fd, "b\n", 2) == 2 ? 0 : 1);
    }
    waitpid(pid, &status, 0);
    write(fd, "c\n", 2);
    check("a forked child writes after its parent, not over it",
          WIFEXITED(status) && WEXITSTATUS(status) == 0 && !strcmp(slurp(OUT), "a\nb\nc\n"));
    check("and the parent's position is past all three", lseek(fd, 0, SEEK_CUR) == 6);
    check("what it was opened for is still known", (fcntl(fd, F_GETFL) & O_ACCMODE) == O_WRONLY);
    close(fd);

    /* cmd < file > file2 */
    pid = fork();
    if (pid == 0) {
        int in = open(OUT, O_RDONLY);
        int out = open("/tmp/redirtest.copy", O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (in < 0 || out < 0 || dup2(in, 0) != 0 || dup2(out, 1) != 1) {
            _exit(112);
        }
        close(in);
        close(out);
        execl(SELF, SELF, "copy", (char *)NULL);
        _exit(111);
    }
    waitpid(pid, &status, 0);
    check("a program reads a file on 0 and writes one on 1",
          WIFEXITED(status) && WEXITSTATUS(status) == 0 &&
              !strcmp(slurp("/tmp/redirtest.copy"), "a\nb\nc\n"));

    /* a | b: two children, one pipe, and the shell holding neither end. */
    int p[2];
    check("a pipe", pipe(p) == 0);
    pid_t left = fork();
    if (left == 0) {
        dup2(p[1], 1);
        close(p[0]);
        close(p[1]);
        execl(SELF, SELF, "say", (char *)NULL);
        _exit(111);
    }
    pid_t right = fork();
    if (right == 0) {
        dup2(p[0], 0);
        close(p[0]);
        close(p[1]);
        execl(SELF, SELF, "count", (char *)NULL);
        _exit(111);
    }
    close(p[0]);
    close(p[1]);
    int ls = -1, rs = -1;
    /* Each by name: the one that ends first is not the one asked about. */
    int got_right = waitpid(right, &rs, 0) == right;
    int got_left = waitpid(left, &ls, 0) == left;
    check("each end of a pipeline is waited for by name", got_right && got_left);
    check("and the reader saw what the writer wrote, then the end",
          WIFEXITED(ls) && WEXITSTATUS(ls) == 0 && WIFEXITED(rs) && WEXITSTATUS(rs) == 5);

    /* > /dev/null */
    pid = fork();
    if (pid == 0) {
        int null = open("/dev/null", O_WRONLY);
        if (null < 0 || dup2(null, 1) != 1) {
            _exit(112);
        }
        execl(SELF, SELF, "say", (char *)NULL);
        _exit(111);
    }
    waitpid(pid, &status, 0);
    check("output sent to /dev/null", WIFEXITED(status) && WEXITSTATUS(status) == 0);

    /* Marked to close on exec, and not. */
    int keep = open(OUT, O_RDONLY);
    int drop = open(OUT, O_RDONLY | O_CLOEXEC);
    char name[4];
    snprintf(name, sizeof name, "%d", keep);
    check("an ordinary descriptor is still open in the program exec'd", run("isopen", name) == 1);
    snprintf(name, sizeof name, "%d", drop);
    check("one marked close-on-exec is not", run("isopen", name) == 0);
    check("and it is still open here", fcntl(drop, F_GETFD) == FD_CLOEXEC);
    check("the mark can be taken off", fcntl(drop, F_SETFD, 0) == 0 && run("isopen", name) == 1);
    close(keep);
    close(drop);

    /* cd dir; cmd */
    check("chdir", chdir("/etc") == 0);
    check("a program run from there starts there", run("where", "/etc") == 0);
    int dirfd = open("/usr", O_RDONLY | O_DIRECTORY);
    check("fchdir", dirfd >= 0 && fchdir(dirfd) == 0);
    char cwd[32];
    check("and getcwd follows", getcwd(cwd, sizeof cwd) && !strcmp(cwd, "/usr"));
    close(dirfd);
    chdir("/");

    /* umask 077; cmd */
    mode_t old = umask(077);
    unlink("/tmp/redirtest.mask");
    struct stat st;
    check("a program run under a umask makes files with it",
          run("make", "/tmp/redirtest.mask") == 0 && stat("/tmp/redirtest.mask", &st) == 0 &&
              (st.st_mode & 0777) == 0600);
    umask(old);

    /* A shell asks whether a child has ended without waiting for it. */
    pid = fork();
    if (pid == 0) {
        execl(SELF, SELF, "sleep", (char *)NULL);
        _exit(111);
    }
    check("waitpid with WNOHANG says nothing has ended yet", waitpid(pid, &status, WNOHANG) == 0);
    check("and then waits for it",
          waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 7);
    check("after which there is nobody to wait for", waitpid(-1, &status, WNOHANG) == -1 && errno == ECHILD);

    /* The names a shell opens for descriptors it already has. */
    int again = open("/dev/stdout", O_WRONLY);
    check("/dev/stdout is another descriptor for 1", again > 2 && write(again, "", 0) == 0);
    close(again);
    check("/dev/fd/N names a descriptor", (again = open("/dev/fd/2", O_WRONLY)) > 2);
    close(again);

    unlink(OUT);
    unlink("/tmp/redirtest.copy");
    unlink("/tmp/redirtest.mask");
    printf("redirtest: %d failed\n", failed);
    return failed ? 1 : 0;
}
