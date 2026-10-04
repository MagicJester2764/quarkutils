/* A mapping a program places itself goes where it said.
 *
 * A dynamic loader maps a library's whole span once, wherever the system
 * likes, and then puts each segment after the first exactly where it belongs
 * inside that span, over what the first mapping put there — MAP_FIXED, over
 * a file's pages and over anonymous memory, the tail of the last segment
 * being memory of its own. The layer used to choose every address itself and
 * ignore the one it was given, so the segments landed somewhere else and the
 * library's code read its data from the wrong place.
 *
 * MAP_FIXED_NOREPLACE says the range must be empty, and is refused where it
 * is not. And the layer's own placing has to step past a range a program
 * took for itself ahead of it, or the next mapping it chose would be refused
 * for landing on it.
 *
 * Exits 0 only if every check holds.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define PAGE 4096

#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static unsigned char *anon(void *at, size_t pages, int flags)
{
    return mmap(at, pages * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | flags, -1, 0);
}

int main(void)
{
    printf("fixedtest:\n");

    /* Four pages, each marked. */
    unsigned char *a = anon(NULL, 4, 0);
    if (a == MAP_FAILED) {
        check("four pages are mapped", 0);
        return 1;
    }
    for (int i = 0; i < 4; i++)
        memset(a + i * PAGE, 'a' + i, PAGE);

    /* The middle two replaced, exactly there. */
    unsigned char *mid = anon(a + PAGE, 2, MAP_FIXED);
    check("MAP_FIXED maps where it is told", mid == a + PAGE);
    check("what was there is gone: the pages are new",
          mid != MAP_FAILED && mid[0] == 0 && mid[2 * PAGE - 1] == 0);
    check("and what was around it is as it was", a[0] == 'a' && a[3 * PAGE] == 'd');

    /* Over something, MAP_FIXED_NOREPLACE is refused, and leaves it be. */
    a[PAGE] = 'x';
    unsigned char *no = anon(a + PAGE, 1, MAP_FIXED_NOREPLACE);
    check("MAP_FIXED_NOREPLACE over a mapping is refused", no == MAP_FAILED && errno == EEXIST);
    check("and what was there is still there", a[PAGE] == 'x');

    /* In a hole it is not. */
    munmap(a + 2 * PAGE, PAGE);
    unsigned char *hole = anon(a + 2 * PAGE, 1, MAP_FIXED_NOREPLACE);
    check("MAP_FIXED_NOREPLACE in a hole maps there", hole == a + 2 * PAGE && hole[0] == 0);

    /* A file's pages, private, over anonymous memory: what a loader does with
       a library's second segment. */
    const char *path = "/tmp/fixedtest.dat";
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    unsigned char page[PAGE];
    for (int i = 0; i < PAGE; i++)
        page[i] = (unsigned char)(i * 7);
    int wrote = fd >= 0 && write(fd, page, PAGE) == PAGE && write(fd, page, PAGE) == PAGE;
    check("a file of two pages is written", wrote);
    unsigned char *f = MAP_FAILED;
    if (wrote)
        f = mmap(a + 3 * PAGE, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_FIXED, fd, PAGE);
    check("its second page is mapped over anonymous memory, where asked", f == a + 3 * PAGE);
    check("and reads as the file does", f != MAP_FAILED && memcmp(f, page, PAGE) == 0);
    if (f != MAP_FAILED) {
        f[0] = 0xEE;
        unsigned char back = 0;
        check("a write to it is the program's own",
              pread(fd, &back, 1, PAGE) == 1 && back == page[0] && f[0] == 0xEE);
    }
    if (fd >= 0)
        close(fd);
    unlink(path);

    /* Ahead of where the layer would map next, and then the layer maps: not
       over it, and not refused for trying to. */
    unsigned char *next = anon(NULL, 1, 0);
    unsigned char *ahead = next == MAP_FAILED ? MAP_FAILED : anon(next + 64 * PAGE, 8, MAP_FIXED);
    check("a range ahead of the layer's next is taken", ahead != MAP_FAILED && ahead == next + 64 * PAGE);
    unsigned char *after = anon(NULL, 128, 0);
    check("and the layer's next mapping is had, and is not on it",
          after != MAP_FAILED && ahead != MAP_FAILED &&
              (after + 128 * PAGE <= ahead || after >= ahead + 8 * PAGE));
    if (after != MAP_FAILED)
        memset(after, 1, 128 * PAGE);
    check("and what was put there first is untouched by it", ahead != MAP_FAILED && ahead[0] == 0);

    /* Addresses no mapping can be at. */
    check("a fixed address off a page boundary is refused", anon(a + 1, 1, MAP_FIXED) == MAP_FAILED && errno == EINVAL);
    check("and one below where programs live",
          anon((void *)0x40000000UL, 1, MAP_FIXED) == MAP_FAILED);

    printf("fixedtest: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
