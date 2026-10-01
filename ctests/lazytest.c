/* Memory is backed when it is touched, not when it is mapped. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define GIB (1024UL * 1024 * 1024)

int main(void) {
    int failed = 0;
    size_t len = 4 * GIB;
    unsigned char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    printf("  %s  four gigabytes, mapped\n", p != MAP_FAILED ? "ok  " : "FAIL");
    failed += p == MAP_FAILED;
    if (p != MAP_FAILED) {
        int ok = 1;
        for (size_t at = 0; at < len; at += 64 * 1024 * 1024) {
            ok &= p[at] == 0;
            p[at] = (unsigned char)(at >> 26);
        }
        for (size_t at = 0; at < len; at += 64 * 1024 * 1024) {
            ok &= p[at] == (unsigned char)(at >> 26);
        }
        printf("  %s  and sixty-four pages of it used\n", ok ? "ok  " : "FAIL");
        failed += !ok;
        FILE *f = fopen("/dev/null", "w");
        int wrote = f && fwrite(p + GIB, 1, 8192, f) == 8192;
        if (f) fclose(f);
        printf("  %s  a page never touched can be written from\n", wrote ? "ok  " : "FAIL");
        failed += !wrote;
        munmap(p, len);
    }
    /* Less than the machine, and far more than one page of it is used. */
    char *big = malloc(64 * 1024 * 1024);
    int used = big != NULL;
    if (big) {
        big[0] = 1;
        big[64 * 1024 * 1024 - 1] = 2;
        used = big[0] == 1 && big[64 * 1024 * 1024 - 1] == 2;
    }
    printf("  %s  malloc of 64 MiB, two bytes of it used\n", used ? "ok  " : "FAIL");
    failed += !used;
    free(big);
    printf("lazytest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
