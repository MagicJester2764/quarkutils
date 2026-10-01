/* A disk as a file: opened under /dev, asked how big it is, and read and
 * written at any offset.
 *
 * It is what a program that makes a filesystem does before it does anything
 * else. The disk is one made of memory for the purpose (`ramdisk 8`), so
 * that nothing depends on what is written to it. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

/* Linux's numbers, which are what a ported program asks with. */
#define BLKRRPART    0x125F
#define BLKGETSIZE   0x1260
#define BLKSSZGET    0x1268
#define BLKGETSIZE64 0x80081272

#define SIZE (8L << 20)

extern char **environ;

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* The RAM disk that was just made: the one of /dev/ram0../dev/ram7 that is
   eight megabytes. Waits a moment for it to appear. */
static int find(char *path, size_t len) {
    for (int tries = 0; tries < 100; tries++) {
        for (int n = 0; n < 8; n++) {
            snprintf(path, len, "/dev/ram%d", n);
            int fd = open(path, O_RDONLY);
            if (fd < 0) {
                continue;
            }
            unsigned long long bytes = 0;
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

int main(void) {
    printf("a disk as a file:\n");
    char path[32];
    char *argv[] = { "ramdisk", "8", NULL };
    pid_t disk = 0;
    check("a disk of memory is started", posix_spawn(&disk, "/usr/bin/ramdisk", NULL, NULL, argv, environ) == 0);
    if (!find(path, sizeof path)) {
        check("and appears under /dev", 0);
        kill(disk, SIGKILL);
        return 1;
    }
    check("and appears under /dev", 1);

    struct stat st;
    check("it is a block device there", stat(path, &st) == 0 && S_ISBLK(st.st_mode));
    int fd = open(path, O_RDWR);
    check("it opens for reading and writing", fd >= 0);
    check("and is one when open", fstat(fd, &st) == 0 && S_ISBLK(st.st_mode));

    unsigned long long bytes = 0;
    unsigned long sectors = 0;
    int sector = 0;
    check("it says how many bytes it has", ioctl(fd, BLKGETSIZE64, &bytes) == 0 && bytes == SIZE);
    check("and how many sectors, of what size",
          ioctl(fd, BLKGETSIZE, &sectors) == 0 && sectors == SIZE / 512 &&
              ioctl(fd, BLKSSZGET, &sector) == 0 && sector == 512);
    check("seeking to its end says the same", lseek(fd, 0, SEEK_END) == SIZE);
    errno = 0;
    check("a question for a terminal is not for a disk",
          isatty(fd) == 0 && errno == ENOTTY);

    /* Across sectors, beginning and ending in the middle of one. */
    unsigned char out[5000], in[5000];
    for (size_t i = 0; i < sizeof out; i++) {
        out[i] = (unsigned char)(i * 7 + 3);
    }
    check("a write across sectors, from the middle of one to the middle of another",
          pwrite(fd, out, 1000, 300) == 1000);
    memset(in, 0xAA, sizeof in);
    check("reads back", pread(fd, in, 1000, 300) == 1000 && !memcmp(in, out, 1000));
    check("and left what was either side of it alone",
          pread(fd, in, 300, 0) == 300 && in[0] == 0 && in[299] == 0 &&
              pread(fd, in, 236, 1300) == 236 && in[0] == 0 && in[235] == 0);
    check("a write longer than a page is all written",
          pwrite(fd, out, sizeof out, 8192 + 17) == (ssize_t)sizeof out &&
              pread(fd, in, sizeof in, 8192 + 17) == (ssize_t)sizeof in && !memcmp(in, out, sizeof in));

    /* Where the disk ends. */
    check("the last sector is written",
          pwrite(fd, out, 512, SIZE - 512) == 512 && pread(fd, in, 512, SIZE - 512) == 512 &&
              !memcmp(in, out, 512));
    check("a read at the end is the end", pread(fd, in, 512, SIZE) == 0);
    errno = 0;
    check("and a write there has nowhere to go", pwrite(fd, out, 512, SIZE) == -1 && errno == ENOSPC);
    check("a read that runs off the end stops at it", pread(fd, in, 4096, SIZE - 100) == 100);

    /* By position, as a program that does not use pread does it. */
    check("read and write move a position",
          lseek(fd, 4096, SEEK_SET) == 4096 && write(fd, "position", 8) == 8 &&
              lseek(fd, 0, SEEK_CUR) == 4104 && lseek(fd, 4096, SEEK_SET) == 4096 &&
              read(fd, in, 8) == 8 && !memcmp(in, "position", 8));

    /* A partition table written by hand, and the disk told to look. */
    unsigned char mbr[512] = { 0 };
    mbr[446 + 4] = 0x83;
    mbr[446 + 8] = 0x00, mbr[446 + 9] = 0x08; /* starts at sector 2048 */
    mbr[446 + 12] = 0x00, mbr[446 + 13] = 0x10; /* 4096 sectors */
    mbr[510] = 0x55, mbr[511] = 0xAA;
    check("a partition table is written and the disk told to read it",
          pwrite(fd, mbr, 512, 0) == 512 && ioctl(fd, BLKRRPART) == 0);
    char part[40];
    snprintf(part, sizeof part, "%sp1", path);
    int pfd = open(part, O_RDWR);
    check("the partition is a device of its own", pfd >= 0 && ioctl(pfd, BLKGETSIZE64, &bytes) == 0 && bytes == 4096 * 512);
    check("whose first byte is the disk's at its start",
          pwrite(pfd, "P", 1, 0) == 1 && pread(fd, in, 1, 2048 * 512) == 1 && in[0] == 'P');
    close(pfd);

    /* A descriptor that only reads is not one a disk is changed through. */
    int ro = open(path, O_RDONLY);
    errno = 0;
    check("a descriptor opened to read does not write",
          ro >= 0 && pwrite(ro, out, 512, 0) == -1 && errno == EBADF);
    errno = 0;
    check("or have the table read again", ioctl(ro, BLKRRPART) == -1 && errno == EACCES);
    close(ro);
    close(fd);

    /* The disk this is running from is one of the disks, and is not written:
       of everything under /dev, something says it is busy, and still opens
       to read. */
    int busy = 0, readable = 0;
    DIR *dev = opendir("/dev");
    struct dirent *e;
    while (dev && (e = readdir(dev))) {
        char other[280];
        if (e->d_type != DT_BLK) {
            continue;
        }
        snprintf(other, sizeof other, "/dev/%s", e->d_name);
        errno = 0;
        int w = open(other, O_RDWR);
        if (w >= 0) {
            close(w);
            continue;
        }
        if (errno != EBUSY) {
            continue;
        }
        busy++;
        int r = open(other, O_RDONLY);
        if (r >= 0 && pread(r, in, 512, 0) == 512) {
            readable++;
        }
        close(r);
    }
    if (dev) {
        closedir(dev);
    }
    check("the disk this runs from is busy to anything that would write it", busy >= 1);
    check("and is read all the same", readable == busy);

    kill(disk, SIGKILL);
    int status = 0;
    check("the disk goes with its server", waitpid(disk, &status, 0) == disk && stat(path, &st) == -1);

    printf("blktest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
