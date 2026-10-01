/* The file calls a ported program leans on, answered by Quark's VFS through
   the Linux translation layer. It removes everything it makes, and tolerates
   whatever a run that stopped half way left behind. */
#define _GNU_SOURCE /* dup3 */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

#define TESTDIR    "/tmp/filetest-a-directory-with-a-name-past-the-old-limit"
#define FILE_A TESTDIR "/a-file-whose-whole-path-is-well-over-forty-seven-bytes"
#define FILE_B TESTDIR "/another"

static int put(const char *path, int flags, const char *text) {
    int fd = open(path, flags, 0644);
    if (fd < 0) {
        return -1;
    }
    ssize_t n = write(fd, text, strlen(text));
    close(fd);
    return n == (ssize_t)strlen(text) ? 0 : -1;
}

/* What a run that stopped part way may have left. Errors are expected. */
static void clear_leftovers(void) {
    unlink(TESTDIR "/sub/inner");
    rmdir(TESTDIR "/sub");
    unlink(TESTDIR "/sub-renamed/inner");
    rmdir(TESTDIR "/sub-renamed");
    unlink(TESTDIR "/gone");
    unlink(TESTDIR "/moved");
    unlink(TESTDIR "/hard");
    unlink(FILE_B);
    DIR *d = opendir(TESTDIR);
    struct dirent *e;
    char path[512];
    while (d && (e = readdir(d))) {
        if (!strncmp(e->d_name, "lock-", 5)) {
            snprintf(path, sizeof path, TESTDIR "/%s", e->d_name);
            unlink(path);
        }
    }
    if (d) {
        closedir(d);
    }
}

int main(void) {
    printf("files:\n");
    clear_leftovers();
    int r = mkdir(TESTDIR, 0755);
    check("make a directory with a long name", r == 0 || errno == EEXIST);
    check("making it again says it exists", mkdir(TESTDIR, 0755) == -1 && errno == EEXIST);
    check("create a file with a long path", put(FILE_A, O_WRONLY | O_CREAT, "0123456789") == 0);

    int fd = open(FILE_A, O_RDWR | O_CREAT, 0644);
    char buf[16] = {0};
    check("O_CREAT opens a file that exists", fd >= 0);
    check("and keeps what it held", fd >= 0 && read(fd, buf, 10) == 10 && !memcmp(buf, "0123456789", 10));
    close(fd);

    fd = open(FILE_A, O_WRONLY | O_CREAT | O_EXCL, 0644);
    check("O_EXCL refuses one that exists", fd == -1 && errno == EEXIST);
    fd = open(FILE_A, O_RDONLY | O_DIRECTORY);
    check("O_DIRECTORY refuses a file", fd == -1 && errno == ENOTDIR);
    fd = open(TESTDIR, O_WRONLY);
    check("a directory cannot be opened to write", fd == -1 && errno == EISDIR);

    struct stat a1, a2, b, d, f;
    put(FILE_B, O_WRONLY | O_CREAT, "b");
    check("stat a file", stat(FILE_A, &a1) == 0 && S_ISREG(a1.st_mode) && a1.st_size >= 10);
    check("twice, and it is the same inode", stat(FILE_A, &a2) == 0 && a1.st_ino == a2.st_ino);
    check("another file is another inode", stat(FILE_B, &b) == 0 && b.st_ino != a1.st_ino);
    check("a file has a link", a1.st_nlink >= 1);
    check("stat a directory", stat(TESTDIR, &d) == 0 && S_ISDIR(d.st_mode) && d.st_nlink >= 2);
    time_t now = time(NULL);
    check("a file written now is dated now", a1.st_mtime <= now && now - a1.st_mtime < 600);
    fd = open(FILE_A, O_RDONLY);
    check("fstat agrees with stat", fd >= 0 && fstat(fd, &f) == 0 && f.st_ino == a1.st_ino);
    close(fd);

    char longname[sizeof TESTDIR + 300];
    memcpy(longname, TESTDIR "/", sizeof TESTDIR);
    memset(longname + sizeof TESTDIR, 'x', 260);
    longname[sizeof TESTDIR + 260] = 0;
    fd = open(longname, O_WRONLY | O_CREAT, 0644);
    check("a name longer than 255 bytes is refused", fd == -1 && errno == ENAMETOOLONG);


    printf("removing, renaming, shortening:\n");
    fd = open(FILE_A, O_WRONLY | O_TRUNC);
    check("O_TRUNC empties a file", fd >= 0 && fstat(fd, &f) == 0 && f.st_size == 0);
    check("which then takes new bytes", fd >= 0 && write(fd, "ab", 2) == 2);
    close(fd);
    check("and holds only those", stat(FILE_A, &f) == 0 && f.st_size == 2);

    check("ftruncate shortens", truncate(FILE_A, 1) == 0 && stat(FILE_A, &f) == 0 && f.st_size == 1);
    check("and lengthens", truncate(FILE_A, 5000) == 0 && stat(FILE_A, &f) == 0 && f.st_size == 5000);
    fd = open(FILE_A, O_RDWR);
    char big[5000];
    memset(big, 1, sizeof big);
    int zeros = fd >= 0 && read(fd, big, sizeof big) == 5000 && big[0] == 'a';
    for (int i = 1; i < 5000 && zeros; i++) {
        zeros = big[i] == 0;
    }
    check("with zeros past the old end", zeros);
    check("and a write into the gap lands", fd >= 0 && lseek(fd, 4096, SEEK_SET) == 4096 && write(fd, "z", 1) == 1);
    close(fd);

    #define GONE TESTDIR "/gone"
    put(GONE, O_WRONLY | O_CREAT, "soon");
    fd = open(GONE, O_RDONLY);
    check("unlink a file", unlink(GONE) == 0);
    check("it is gone", stat(GONE, &f) == -1 && errno == ENOENT);
    memset(buf, 0, sizeof buf);
    check("an open handle still reads it", fd >= 0 && read(fd, buf, 4) == 4 && !memcmp(buf, "soon", 4));
    close(fd);
    check("unlinking it again says so", unlink(GONE) == -1 && errno == ENOENT);
    check("unlink refuses a directory", unlink(TESTDIR) == -1 && errno == EISDIR);
    #define HARD TESTDIR "/hard"
    struct stat h1, h2;
    check("link makes a second name", link(FILE_A, HARD) == 0);
    check("for the same file", stat(FILE_A, &h1) == 0 && stat(HARD, &h2) == 0 &&
          h1.st_ino == h2.st_ino && h1.st_nlink == 2);
    fd = open(HARD, O_WRONLY);
    check("a write through one name", fd >= 0 && pwrite(fd, "L", 1, 0) == 1);
    close(fd);
    fd = open(FILE_A, O_RDONLY);
    check("shows through the other", fd >= 0 && read(fd, buf, 1) == 1 && buf[0] == 'L');
    close(fd);
    check("unlinking one name", unlink(HARD) == 0);
    check("leaves the other", stat(FILE_A, &h1) == 0 && h1.st_nlink == 1);
    check("link refuses a directory", link(TESTDIR, HARD) == -1 && errno == EPERM);
    check("and a name that is taken", link(FILE_A, FILE_B) == -1 && errno == EEXIST);

    #define MOVED TESTDIR "/moved"
    unlink(MOVED);
    struct stat before;
    stat(FILE_B, &before);
    check("rename a file", rename(FILE_B, MOVED) == 0);
    check("the old name is gone", stat(FILE_B, &f) == -1 && errno == ENOENT);
    check("the new one is the same file", stat(MOVED, &f) == 0 && f.st_ino == before.st_ino);
    put(FILE_B, O_WRONLY | O_CREAT, "replacement");
    check("rename over a file replaces it", rename(MOVED, FILE_B) == 0 && stat(FILE_B, &f) == 0 && f.st_ino == before.st_ino && f.st_size == 1);

    #define SUB TESTDIR "/sub"
    #define SUB2 TESTDIR "/sub-renamed"
    mkdir(SUB, 0755);
    put(SUB "/inner", O_WRONLY | O_CREAT, "x");
    check("rmdir refuses a directory with something in it", rmdir(SUB) == -1 && errno == ENOTEMPTY);
    check("rename a directory", rename(SUB, SUB2) == 0 && stat(SUB2 "/inner", &f) == 0);
    check("not into itself", rename(SUB2, SUB2 "/itself") == -1 && errno == EINVAL);
    check("empty it", unlink(SUB2 "/inner") == 0);
    check("then rmdir removes it", rmdir(SUB2) == 0 && stat(SUB2, &f) == -1 && errno == ENOENT);

    printf("second descriptors:\n");
    fd = open(FILE_A, O_RDWR);
    int copy = fd >= 0 ? dup(fd) : -1;
    check("dup a file", copy >= 0 && copy != fd);
    check("the copy shares the position", copy >= 0 && lseek(fd, 2, SEEK_SET) == 2 && lseek(copy, 0, SEEK_CUR) == 2);
    close(fd);
    check("and keeps the file open when the first closes", copy >= 0 && read(copy, buf, 3) == 3);
    int third = copy >= 0 ? fcntl(copy, F_DUPFD_CLOEXEC, 40) : -1;
    check("F_DUPFD_CLOEXEC takes a number at or above the one asked", third >= 40);
    int other = open(FILE_B, O_RDONLY);
    check("dup2 replaces what the target named", other >= 0 && third >= 0 && dup2(other, third) == third && lseek(third, 0, SEEK_END) == 1);
    check("and the first file's copy still names it", copy >= 0 && lseek(copy, 0, SEEK_END) == 5000);
    check("dup3 refuses to copy a descriptor onto itself", other >= 0 && dup3(other, other, 0) == -1 && errno == EINVAL);
    close(copy);
    close(third);
    close(other);
    check("a descriptor that was closed is gone", fcntl(copy, F_GETFD) == -1 && errno == EBADF);
    char lock[] = TESTDIR "/lock-XXXXXX";
    int made = mkstemp(lock);
    int kept = made >= 0 ? fcntl(made, F_DUPFD_CLOEXEC, STDIN_FILENO) : -1;
    if (made >= 0) {
        close(made);
    }
    check("mkstemp and a close-on-exec copy, as fontconfig makes a lock",
          kept >= 0 && write(kept, "1\n", 2) == 2 && close(kept) == 0 && unlink(lock) == 0);

    check("tidy up", unlink(FILE_A) == 0 && unlink(FILE_B) == 0 && rmdir(TESTDIR) == 0);

    printf("filetest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
