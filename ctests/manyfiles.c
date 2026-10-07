/* Nine hundred files open at once, and five hundred of them mapped.
 *
 * Every open file is a handle in its file server's table, and every mapped
 * one a memory object it pages and a capability it keeps: the table had 512
 * handles for every program there was, and the server room for 192 mapped
 * files. A desktop's programs — a browser, a compiler, a session of a few
 * dozen — open more than that between them, and rustc maps every object
 * file of a crate to make an archive.
 *
 * A directory of 900 small files in /tmp: all of them opened, and read; 500
 * of them mapped, and a byte of each read from the mapping; then all
 * closed and removed, and given back once nothing holds them.
 *
 * Exits 0 only if every check holds.
 */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <time.h>
#include <unistd.h>

#define FILES 900
#define MAPPED 500
#define DIR "/tmp/manyfiles"

static int failures;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failures++;
    }
}

/* The files /tmp's filesystem has free. */
static long free_files(void) {
    struct statvfs fs;
    return statvfs("/tmp", &fs) == 0 ? (long)fs.f_ffree : -1;
}

static void name(int i, char *out, size_t room) {
    snprintf(out, room, DIR "/f%03d", i);
}

/* What file `i` holds: its number, said so that no two are alike. */
static int contents(int i, char *out, size_t room) {
    return snprintf(out, room, "file %d of %d\n", i, FILES);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("manyfiles:\n");
    static int fds[FILES];
    static unsigned char *maps[MAPPED];
    char path[64], want[64], got[64];

    /* Left by a run that did not finish. */
    for (int i = 0; i < FILES; i++) {
        name(i, path, sizeof path);
        unlink(path);
    }
    rmdir(DIR);
    long before = free_files();

    int made = 0;
    if (mkdir(DIR, 0755) == 0) {
        for (; made < FILES; made++) {
            name(made, path, sizeof path);
            int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
            int len = contents(made, want, sizeof want);
            int wrote = fd >= 0 && write(fd, want, (size_t)len) == len;
            if (fd >= 0) {
                close(fd);
            }
            if (!wrote) {
                break;
            }
        }
    }
    check("a directory of 900 files is made", made == FILES);

    int opened = 0;
    for (; opened < made; opened++) {
        name(opened, path, sizeof path);
        fds[opened] = open(path, O_RDONLY);
        if (fds[opened] < 0) {
            break;
        }
    }
    check("all 900 are open at once", opened == FILES);
    if (opened != FILES) {
        printf("  (%d opened)\n", opened);
    }

    int read_back = 0;
    for (int i = 0; i < opened; i++) {
        int len = contents(i, want, sizeof want);
        if (pread(fds[i], got, sizeof got, 0) == len && memcmp(got, want, (size_t)len) == 0) {
            read_back++;
        }
    }
    check("and each reads as it was written", read_back == FILES);

    int mapped = 0;
    for (; mapped < MAPPED && mapped < opened; mapped++) {
        maps[mapped] = mmap(0, 4096, PROT_READ, MAP_PRIVATE, fds[mapped], 0);
        if (maps[mapped] == MAP_FAILED) {
            break;
        }
    }
    check("500 of them are mapped at once", mapped == MAPPED);
    if (mapped != MAPPED) {
        printf("  (%d mapped)\n", mapped);
    }

    int from_maps = 0;
    for (int i = 0; i < mapped; i++) {
        int len = contents(i, want, sizeof want);
        if (memcmp(maps[i], want, (size_t)len) == 0 && maps[i][len] == 0) {
            from_maps++;
        }
    }
    check("and each mapping reads as the file was written", from_maps == MAPPED);

    for (int i = 0; i < mapped; i++) {
        munmap(maps[i], 4096);
    }
    int closed = 0;
    for (int i = 0; i < opened; i++) {
        closed += close(fds[i]) == 0;
    }
    int removed = 0;
    for (int i = 0; i < made; i++) {
        name(i, path, sizeof path);
        removed += unlink(path) == 0;
    }
    int gone = rmdir(DIR) == 0;
    check("all are closed and removed", closed == opened && removed == made && gone);

    /* Removed, and held by nothing: each is its server's to let go, once it
       has been told the last descriptor and the last mapping have gone. */
    long after = free_files();
    for (int tries = 0; after != before && tries < 1000; tries++) {
        struct timespec ten_ms = {0, 10 * 1000 * 1000};
        nanosleep(&ten_ms, 0);
        after = free_files();
    }
    check("and given back, all of them", before >= 0 && after == before);
    if (before < 0 || after != before) {
        printf("  (%ld files free, %ld before)\n", after, before);
    }

    printf("manyfiles: %d failed\n", failures);
    return failures ? 1 : 0;
}
