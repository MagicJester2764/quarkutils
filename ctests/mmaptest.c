/* A mapping bigger than the machine is refused, and the refusal leaves
 * nothing behind.
 *
 * Anonymous memory is given its frames when it is touched, but a single
 * mapping bigger than all of memory is refused when it is made, as Linux's
 * overcommit heuristic refuses it without MAP_NORESERVE. pixman asks for such
 * things — a trapezoid mask the size of its whole destination, drawn into one
 * corner — and copes, because it checks; and a calloc of one has to come back
 * NULL, or the C library reads all of it looking for zeroes. What pixman could
 * not cope with, once, was a refusal that kept what it had taken on the way:
 * the address the next mapping was going to use was gone, so every mapping
 * after it failed as well.
 *
 * Nor one that crossed a line every two gigabytes. pixman's stress test
 * reaches an image through its address with bits 63 and 31 turned over, adds
 * the offset to that, and turns them back: which is the address only while
 * the image does not cross such a line. Now and then one did — the arena
 * only goes up, from a random start, and the test's masks carry it on in
 * steps of gigabytes — and the test read four gigabytes from where it meant
 * to.
 *
 * Exits 0 only if every check holds.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

#define MiB ((size_t)1 << 20)
#define GiB ((size_t)1 << 30)

static int failed;

static void check(const char *what, int ok)
{
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok)
        failed = 1;
}

static void *map(size_t len)
{
    return mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
}

/* Map `len`, write every byte of it, and give it back. */
static int use(size_t len)
{
    unsigned char *p = map(len);
    if (p == MAP_FAILED)
        return 0;
    memset(p, 0xA5, len);
    int ok = p[0] == 0xA5 && p[len - 1] == 0xA5;
    return munmap(p, len) == 0 && ok;
}

/* Whether `count` mappings of `len` bytes, each smaller than two gigabytes,
   all fit between two of the lines. */
static int none_crosses(size_t len, int count)
{
    const unsigned long line = 2 * GiB;
    void *maps[64];
    int ok = 1, made = 0;
    for (int i = 0; i < count && i < 64; i++) {
        maps[i] = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
        if (maps[i] == MAP_FAILED) {
            ok = 0;
            break;
        }
        made++;
        unsigned long at = (unsigned long)maps[i];
        if (at / line != (at + len - 1) / line)
            ok = 0;
    }
    for (int i = 0; i < made; i++)
        munmap(maps[i], len);
    return ok;
}

static int malloc_works(size_t len)
{
    char *p = malloc(len);
    if (!p)
        return 0;
    memset(p, 1, len);
    free(p);
    return 1;
}

int main(void)
{
    /* More than any machine this runs on, but inside the range the C library
       hands out addresses from — so it is the kernel that says no, and not a
       bounds check in the library. */
    check("64 GiB is refused", map(64 * GiB) == MAP_FAILED);
    check("a megabyte can still be mapped and used", use(MiB));
    check("64 GiB is refused again", map(64 * GiB) == MAP_FAILED);
    check("and 32 MiB still fits after it", use(32 * MiB));
    /* How pixman asked. */
    check("calloc of 64 GiB is NULL", calloc(1, 64 * GiB) == NULL);
    check("and malloc works after it", malloc_works(4 * MiB));
    /* Ten gigabytes of them is four or five lines, from wherever the arena
       began. */
    check("no mapping smaller than two gigabytes crosses a line every two",
          none_crosses(256 * MiB + 4096, 40));
    puts(failed ? "mmaptest: FAIL" : "mmaptest: ok");
    return failed;
}
