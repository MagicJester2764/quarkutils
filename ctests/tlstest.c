/* Thread-local storage, as a C program sees it.
 *
 * musl finds a static program's TLS template through the auxiliary vector:
 * AT_PHDR says where the program headers are, and one of them is PT_TLS. A
 * spawner that does not say leaves musl allocating every thread's TLS block
 * with no room for the program's own thread-locals — and on x86-64 those live
 * *below* the thread pointer, so writing one writes over whatever memory comes
 * before the block.
 *
 * musl itself never notices, because it keeps errno and friends in its thread
 * structure rather than in .tbss. pixman was the first program here with real
 * thread-locals, and its composite cache — which lives in one — sent every
 * lookup to the wrong function.
 *
 * Exits 0 only if every check holds.
 */
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <elf.h>

#define N 384 /* pixman's .tbss is 0x180; the same size, on purpose */

static _Thread_local unsigned char tls_block[N];
static _Thread_local int tls_int = 7; /* .tdata: needs the template copied in */

static int failures;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failures++;
    }
}

static void fill(unsigned char seed) {
    for (int i = 0; i < N; i++) {
        tls_block[i] = (unsigned char)(seed + i * 7);
    }
}

static int intact(unsigned char seed) {
    for (int i = 0; i < N; i++) {
        if (tls_block[i] != (unsigned char)(seed + i * 7)) {
            return 0;
        }
    }
    return 1;
}

static int thread_ok;

static void *worker(void *arg) {
    (void)arg;
    /* A new thread starts with the template's values, not the creator's. */
    int fresh = tls_int == 7;
    fill(0xB0);
    tls_int = 99;
    thread_ok = fresh && intact(0xB0) && tls_int == 99;
    return NULL;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("tls:\n");

    unsigned long phdr = getauxval(AT_PHDR);
    unsigned long phnum = getauxval(AT_PHNUM);
    check("the aux vector says where the program headers are", phdr != 0 && phnum != 0);
    int has_tls = 0;
    if (phdr && phnum) {
        const Elf64_Phdr *p = (const Elf64_Phdr *)phdr;
        for (unsigned long i = 0; i < phnum; i++) {
            if (p[i].p_type == PT_TLS) {
                has_tls = 1;
            }
        }
    }
    check("and one of them is the TLS template", has_tls);

    check(".tdata starts with its initial value", tls_int == 7);

    /* The thread pointer is the pthread structure; on x86-64 the thread-locals
       are below it and must be inside the block musl allocated. */
    unsigned char *tp = (unsigned char *)pthread_self();
    unsigned char *lo = (unsigned char *)&tls_block[0];
    check("thread-locals are inside the TLS block", lo < tp && (unsigned long)(tp - lo) <= 4096);

    fill(0xA0);
    tls_int = 42;
    /* Anything that might use the memory just below the thread pointer. */
    char buf[64];
    snprintf(buf, sizeof buf, "%d %s", 12345, "formatting");
    check("formatting still works after writing them", strcmp(buf, "12345 formatting") == 0);

    pthread_t t;
    int made = pthread_create(&t, NULL, worker, NULL) == 0;
    check("a thread starts", made);
    if (made) {
        pthread_join(t, NULL);
    }
    check("the thread had its own copy", thread_ok);
    check("and ours survived it", intact(0xA0) && tls_int == 42);

    printf("tls: %d failed\n", failures);
    return failures ? 1 : 0;
}
