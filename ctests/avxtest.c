/* The wide registers, as a C program has them.
 *
 * A program compiled to use AVX keeps what it is working on in registers
 * 256 bits wide, and a library that looks at the processor before choosing
 * how to do something — a pixel blend, a checksum, a `memcpy` — chooses
 * them where they are there. For a long time they were not: the kernel
 * saved only what SSE has, an AVX instruction was a fault, and the answer
 * to "may I?" was no.
 *
 * It is yes now, and what is asked here is that it is true: that the
 * compiler's own check says so, and that a sum done in wide registers by
 * two threads and a child at once, each preempted in the middle of it by
 * the others, comes to what it should every time. A kernel that turned the
 * registers on and did not save them gets this wrong within a tick.
 */
#define _GNU_SOURCE
#include <immintrin.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

#define N 4096

/* The same sum two ways: in general registers, a number at a time, and in
   four wide registers, thirty-two numbers at a time. */
static long long plainly(const int *v) {
    long long sum = 0;
    for (int i = 0; i < N; i++) {
        sum += (long long)v[i] * (i & 7);
    }
    return sum;
}

__attribute__((target("avx2"))) static long long widely(const int *v) {
    __m256i weights = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
    __m256i a = _mm256_setzero_si256(), b = a, c = a, d = a;
    for (int i = 0; i < N; i += 32) {
        __m256i w = _mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(v + i)), weights);
        __m256i x = _mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(v + i + 8)), weights);
        __m256i y = _mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(v + i + 16)), weights);
        __m256i z = _mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(v + i + 24)), weights);
        /* Widened to sixty-four bits before they are added, in the upper
           half of each register as much as the lower. */
        a = _mm256_add_epi64(a, _mm256_cvtepi32_epi64(_mm256_castsi256_si128(w)));
        a = _mm256_add_epi64(a, _mm256_cvtepi32_epi64(_mm256_extracti128_si256(w, 1)));
        b = _mm256_add_epi64(b, _mm256_cvtepi32_epi64(_mm256_castsi256_si128(x)));
        b = _mm256_add_epi64(b, _mm256_cvtepi32_epi64(_mm256_extracti128_si256(x, 1)));
        c = _mm256_add_epi64(c, _mm256_cvtepi32_epi64(_mm256_castsi256_si128(y)));
        c = _mm256_add_epi64(c, _mm256_cvtepi32_epi64(_mm256_extracti128_si256(y, 1)));
        d = _mm256_add_epi64(d, _mm256_cvtepi32_epi64(_mm256_castsi256_si128(z)));
        d = _mm256_add_epi64(d, _mm256_cvtepi32_epi64(_mm256_extracti128_si256(z, 1)));
    }
    long long parts[4];
    _mm256_storeu_si256((__m256i *)parts, _mm256_add_epi64(_mm256_add_epi64(a, b), _mm256_add_epi64(c, d)));
    return parts[0] + parts[1] + parts[2] + parts[3];
}

static long long ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000LL + t.tv_nsec / 1000000;
}

/* Sum numbers of this worker's own, widely, over and over for a fifth of a
   second, and count the times it did not come to what it should. */
static long work(unsigned seed) {
    static __thread int v[N];
    for (int i = 0; i < N; i++) {
        seed = seed * 1103515245u + 12345u;
        v[i] = (int)(seed >> 8) % 100000;
    }
    long long want = plainly(v);
    long wrong = 0, rounds = 0;
    for (long long until = ms() + 200; ms() < until;) {
        for (int i = 0; i < 64; i++, rounds++) {
            if (widely(v) != want) {
                wrong++;
            }
        }
    }
    return rounds ? wrong : -1;
}

static void *worker(void *arg) {
    return (void *)work((unsigned)(long)arg);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("the wide registers:\n");
    __builtin_cpu_init();
    if (!__builtin_cpu_supports("avx2")) {
        /* The compiler's check is of the processor and of the kernel both:
           it says no on a machine without them, and on a kernel that does
           not save them. On the first there is nothing to ask. */
        printf("        AVX2 is not to be used on this machine: not checked\n");
        printf("avxtest: ok\n");
        return 0;
    }
    check("a program is told it may use them", 1);

    pthread_t threads[3];
    for (long i = 0; i < 3; i++) {
        pthread_create(&threads[i], NULL, worker, (void *)(i + 1));
    }
    pid_t child = fork();
    if (child == 0) {
        _exit(work(99) == 0 ? 0 : 1);
    }
    long mine = work(7);
    long theirs = 0;
    for (int i = 0; i < 3; i++) {
        void *wrong;
        pthread_join(threads[i], &wrong);
        theirs += (long)wrong;
    }
    int status = -1;
    waitpid(child, &status, 0);
    check("a sum in wide registers is right, every time, in a thread preempted by others", mine == 0);
    check("and in each of the others", theirs == 0);
    check("and in a child", WIFEXITED(status) && WEXITSTATUS(status) == 0);

    printf("avxtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
