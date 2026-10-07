/* What this layer keeps about a descriptor, beside the kernel's table.
 *
 * It was arrays and words of sixty-four, because the kernel's table was
 * sixty-four. A program's descriptors go to its limit now — 1,024 to start,
 * 65,536 at most — and a word of sixty-four bits cannot say which of them are
 * non-blocking. So each descriptor has a record, in a page of them taken from
 * the arena the first time one of them is written about and kept for as long
 * as the program is. A descriptor never written about reads as noughts, which
 * is what a record starts as: not yet asked about, waiting, no timeouts.
 *
 * Making a page is not a call to malloc: this is reached from inside system
 * calls, a signal handler's among them. */
#include <quark/syscall.h>

#include "abi.h"

#define PER_PAGE (4096 / sizeof(struct __quark_side))
#define PAGES ((MAX_FDS + PER_PAGE - 1) / PER_PAGE)

static struct __quark_side *pages[PAGES];
static int lock;

struct __quark_side *__quark_side_if(long fd) {
    if (fd < 0 || fd >= MAX_FDS) {
        return 0;
    }
    struct __quark_side *page = __atomic_load_n(&pages[(unsigned long)fd / PER_PAGE], __ATOMIC_ACQUIRE);
    return page ? &page[(unsigned long)fd % PER_PAGE] : 0;
}

struct __quark_side *__quark_side(long fd) {
    if (fd < 0 || fd >= MAX_FDS) {
        return 0;
    }
    unsigned long p = (unsigned long)fd / PER_PAGE;
    struct __quark_side *page = __atomic_load_n(&pages[p], __ATOMIC_ACQUIRE);
    if (!page) {
        __quark_lock(&lock);
        page = pages[p];
        if (!page) {
            page = __quark_pages(1);
            __atomic_store_n(&pages[p], page, __ATOMIC_RELEASE);
        }
        __quark_unlock(&lock);
        if (!page) {
            return 0;
        }
    }
    return &page[(unsigned long)fd % PER_PAGE];
}

/* A forked child has only the thread that forked: whoever held the lock is
   in the parent. */
void __quark_side_forked(void) {
    lock = 0;
}
