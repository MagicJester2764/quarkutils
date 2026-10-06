/* Forty files mapped at once.
 *
 * A mapped file is a memory object its file server pages, and the server
 * kept each object's capability in a slot of its own: thirty of them, in a
 * capability space of sixty-four. rustc maps every object file of a crate
 * to put them in an archive, and the libraries it reads, and cargo runs
 * four rustc at once: building the standard library on Quark, the
 * thirty-first map was refused, as "No file descriptors available".
 *
 * And forty given up at once: the kernel told a file's server that nothing
 * mapped one of its files any more in a list thirty-two long, and the eight
 * it had no room for were never told. Their server kept them, removed and
 * open, for as long as the machine was up — eight files on the orphan list
 * of every disk this test had run on.
 *
 * Exits 0 only if every check holds.
 */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/statvfs.h>
#include <time.h>
#include <unistd.h>

/* The files /tmp's filesystem has free. */
static long free_files(void)
{
    struct statvfs fs;
    return statvfs("/tmp", &fs) == 0 ? (long)fs.f_ffree : -1;
}

#define FILES 40

int main(void)
{
    char path[64];
    unsigned char *maps[FILES];
    int made = 0, mapped = 0, read_back = 0;
    long before = free_files();
    for (int i = 0; i < FILES; i++) {
        snprintf(path, sizeof path, "/tmp/manymaps-%02d", i);
        int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
        if (fd < 0)
            break;
        unsigned char page[4096];
        memset(page, 'a' + i % 26, sizeof page);
        if (write(fd, page, sizeof page) == (ssize_t)sizeof page)
            made++;
        maps[i] = mmap(0, sizeof page, PROT_READ, MAP_PRIVATE, fd, 0);
        close(fd);
        if (maps[i] == MAP_FAILED)
            break;
        mapped++;
    }
    for (int i = 0; i < mapped; i++)
        if (maps[i][0] == 'a' + i % 26 && maps[i][4095] == 'a' + i % 26)
            read_back++;
    printf("  %s  forty files are mapped at once (%d of %d)\n", mapped == FILES ? "ok  " : "FAIL", mapped, FILES);
    printf("  %s  and each reads as it was written\n", read_back == FILES ? "ok  " : "FAIL");
    for (int i = 0; i < mapped; i++)
        munmap(maps[i], 4096);
    for (int i = 0; i < made; i++) {
        snprintf(path, sizeof path, "/tmp/manymaps-%02d", i);
        unlink(path);
    }
    /* Removed, and mapped by nothing: each is its server's to let go, once
       it has been told. A few seconds for that. */
    long after = free_files();
    for (int tries = 0; after != before && tries < 300; tries++) {
        struct timespec ten_ms = {0, 10 * 1000 * 1000};
        nanosleep(&ten_ms, 0);
        after = free_files();
    }
    int given_back = before >= 0 && after == before;
    printf("  %s  and all forty are given back once nothing maps them (%ld of %ld free, %ld before)\n",
           given_back ? "ok  " : "FAIL", after, before, before);
    int ok = mapped == FILES && read_back == FILES && given_back;
    printf("manymaps: %s\n", ok ? "passed" : "FAILED");
    return !ok;
}
