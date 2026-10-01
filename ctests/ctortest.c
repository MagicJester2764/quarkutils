/* Constructors, which no C program on Quark ever ran.
 *
 * musl runs the functions between __init_array_start and __init_array_end, and
 * declares both weak. A compiler that puts constructors in .ctors instead —
 * which ours did, having been configured without --enable-initfini-array — and
 * a link script that never defines the two symbols together make a C library
 * that walks an empty range and says nothing. pixman builds its whole
 * implementation table in a constructor, so every composite it attempted
 * reported that no function could do it.
 *
 * Priorities are checked as well as presence: constructor(101) must run before
 * constructor(102), which is what SORT_BY_INIT_PRIORITY in the link script is
 * for.
 */
#include <stdio.h>

static int order[3];
static int n;

__attribute__((constructor(101))) static void first(void)  { order[n++] = 1; }
__attribute__((constructor(102))) static void second(void) { order[n++] = 2; }
__attribute__((constructor))      static void plain(void)  { order[n++] = 3; }

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    int ran = n == 3;
    int prioritised = order[0] == 1 && order[1] == 2;
    printf("ctor: all ran %s, in priority order %s\n",
           ran ? "ok" : "FAIL", prioritised ? "ok" : "FAIL");
    return ran && prioritised ? 0 : 1;
}
