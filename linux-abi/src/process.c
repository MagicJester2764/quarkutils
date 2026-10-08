/* Replacing a program with another one, which is what `exec` is.
 *
 * Quark loads programs in user space: a spawner reads an ELF, builds the image
 * in its own memory and moves the pages into the address space the new program
 * will run in. `quark_rt::spawn` does that in Rust for a *child*; this does the
 * same in C for the caller itself, and then asks the kernel for the one thing
 * user space cannot do — swap the address space under a running task.
 *
 * The task is the same task afterwards: same id, same descriptors, same
 * capabilities, same parent, same working directory. What changes is the
 * program, which is exactly what `execve` promises.
 *
 * Everything the program has open survives, files included, because a file
 * is a descriptor in the kernel's table and the table is what the kernel
 * keeps; what was marked to close on exec is closed by it. Nothing of that
 * is this file's doing. It used not to be true of files: they lived in the
 * memory being replaced, under numbers this layer made up, and a shell could
 * not redirect a child's output into one.
 *
 * What does not survive is what this layer remembers in its own memory:
 * which descriptors were asked not to wait. A program is exec'd into holding
 * descriptors that wait.
 */

#include <quark/layout.h>
#include <quark/syscall.h>

#include "abi.h"

/* Who this is, to anything that will ask about it later. */
long __quark_getpid(void) {
    return (long)__syscall1(SYS_PID, 0);
}

/* fork: a copy of this program, told apart by the answer. The parent is told
   the child's process id and not its task id: the task id is a slot, and the
   slot a child leaves is the one the next child is given. bash, told the
   same number twice, took a command for the background job it had last
   started and did not wait for it. */
long __quark_fork(void) {
    unsigned long child = __syscall0(SYS_FORK);
    if (child == QUARK_ERR) {
        return -LX_EAGAIN;
    }
    if (child == 0) {
        /* One thread, in a copy of the memory as it stood: a lock another
           thread held when the copy was taken is held by nobody here. */
        __quark_sig_forked();
        __quark_locks_forked();
        return 0;
    }
    unsigned long pid = __syscall1(SYS_PID, child);
    return pid == QUARK_ERR ? (long)child : (long)pid;
}

void __quark_locks_forked(void) {
    __quark_arena_forked();
    __quark_side_forked();
    __quark_poll_forked();
}

#define PAGE_SIZE 4096UL
#define EHDR_SIZE 64UL
#define PHDR_SIZE 56UL
#define PT_LOAD 1U
#define PT_INTERP 3U
#define PT_PHDR 6U
#define ET_DYN 3
#define MAX_SEGMENTS 8
/* As `quark_rt::spawn`: a program may span a gigabyte from its first page to
   its last, which is what has to be free at the staging address. */
#define MAX_IMAGE_SPAN (1UL << 30)
#define STACK_PAGES 256UL
#define MAP_CHUNK 256UL

/* Where the image, the stack and the argument page are built before they are
   moved: clear of the heap, of the layer's anonymous arena and of anything a
   program is loaded at, and a terabyte apart because an image may be a
   gigabyte wide. `layout.h` asserts they are user addresses. */
#define STAGE_ELF    QUARK_STAGE_ELF
#define STAGE_STACK  QUARK_STAGE_STACK
#define STAGE_ARGS   QUARK_STAGE_ARGS
#define STAGE_INTERP QUARK_STAGE_INTERP

struct ehdr {
    unsigned char e_ident[16];
    unsigned short e_type;
    unsigned short e_machine;
    unsigned int e_version;
    unsigned long e_entry;
    unsigned long e_phoff;
    unsigned long e_shoff;
    unsigned int e_flags;
    unsigned short e_ehsize;
    unsigned short e_phentsize;
    unsigned short e_phnum;
    unsigned short e_shentsize;
    unsigned short e_shnum;
    unsigned short e_shstrndx;
};

struct phdr {
    unsigned int p_type;
    unsigned int p_flags;
    unsigned long p_offset;
    unsigned long p_vaddr;
    unsigned long p_paddr;
    unsigned long p_filesz;
    unsigned long p_memsz;
    unsigned long p_align;
};

struct segment {
    unsigned long first;   /* the page its first byte is in */
    unsigned long end;     /* one past the page its last byte is in */
    unsigned long vaddr;
    unsigned long vend;
    unsigned long offset;
    unsigned long filesz;
    int writable;
};

static void unmap_stage(unsigned long at, unsigned long pages) {
    unsigned long done = 0;
    while (done < pages) {
        unsigned long n = pages - done;
        if (n > MAP_CHUNK) {
            n = MAP_CHUNK;
        }
        __syscall2(SYS_MUNMAP, at + done * PAGE_SIZE, n);
        done += n;
    }
}

/* Fresh, zeroed pages at `at`, or nothing at all. */
static int map_stage(unsigned long at, unsigned long pages) {
    unsigned long done = 0;
    while (done < pages) {
        unsigned long n = pages - done;
        if (n > MAP_CHUNK) {
            n = MAP_CHUNK;
        }
        if (__syscall2(SYS_MMAP, at + done * PAGE_SIZE, n) == QUARK_ERR) {
            unmap_stage(at, done);
            return 0;
        }
        done += n;
    }
    return 1;
}

/* Move staged pages into the new address space. They are the new space's from
   then on: it frees them when it goes, and this one keeps no way to reach
   them. */
static int give(unsigned long cr3, unsigned long there, unsigned long here,
                unsigned long pages, int writable) {
    unsigned long done = 0;
    while (done < pages) {
        unsigned long n = pages - done;
        if (n > MAP_CHUNK) {
            n = MAP_CHUNK;
        }
        if (__syscall5(SYS_ADDRSPACE_GIVE, cr3, there + done * PAGE_SIZE,
                       here + done * PAGE_SIZE, n, writable ? 1UL : 0UL) == QUARK_ERR) {
            return 0;
        }
        done += n;
    }
    return 1;
}

/* Read the loadable segments, checked the way `quark_rt::spawn` checks them:
   in address order, apart, inside the file, and not spanning more than a
   gigabyte. Each is where the file says plus `bias`: nought for a program
   linked to be somewhere, and where this put it for one linked to be put
   anywhere (a PIE) or for an interpreter. Returns how many, or -1. */
static int read_segments(const struct phdr *ph, int phnum, unsigned long file_size,
                         struct segment *out, unsigned long bias) {
    int n = 0;
    for (int i = 0; i < phnum; i++) {
        if (ph[i].p_type != PT_LOAD || ph[i].p_memsz == 0) {
            continue;
        }
        unsigned long vaddr = ph[i].p_vaddr + bias;
        unsigned long memsz = ph[i].p_memsz;
        unsigned long filesz = ph[i].p_filesz;
        unsigned long offset = ph[i].p_offset;
        if (filesz > memsz || offset + filesz < offset || offset + filesz > file_size) {
            return -1;
        }
        if (vaddr < QUARK_USER_MIN) {
            return -1; /* below PML4[1] is the kernel's, whatever the file says */
        }
        struct segment s;
        s.first = vaddr & ~(PAGE_SIZE - 1);
        s.end = (vaddr + memsz + PAGE_SIZE - 1) & ~(PAGE_SIZE - 1);
        s.vaddr = vaddr;
        s.vend = vaddr + memsz;
        s.offset = offset;
        s.filesz = filesz;
        s.writable = (ph[i].p_flags & 2) != 0;
        if (n > 0 && s.vaddr < out[n - 1].vend) {
            return -1;
        }
        unsigned long base = (n == 0) ? s.first : out[0].first;
        if (n == MAX_SEGMENTS || s.end - base > MAX_IMAGE_SPAN) {
            return -1;
        }
        out[n++] = s;
    }
    return n == 0 ? -1 : n;
}

/* Write the argument page: the arguments, then the environment, then the
   program's own header table at the end.
 *
 * The table is not optional. A program's headers are in no segment it loads,
 * and musl finds its thread-local template through them; without them every
 * thread-local in a C program lands outside its block. The end of the page
 * belongs to them however long the command line is. */
static void build_args(unsigned char *page, char *const argv[], char *const envp[],
                       const struct phdr *ph, int phnum, unsigned long moved) {
    unsigned long off = 0;
    for (int section = 0; section < 2; section++) {
        char *const *list = section == 0 ? argv : envp;
        unsigned long count_at = off;
        off += 8;
        unsigned long written = 0;
        for (int i = 0; list && list[i]; i++) {
            unsigned long len = 0;
            while (list[i][len]) {
                len++;
            }
            if (off + 8 + len > (unsigned long)QUARK_PHDRS_AT) {
                break;
            }
            *(unsigned long *)(page + off) = len;
            off += 8;
            for (unsigned long j = 0; j < len; j++) {
                page[off + j] = (unsigned char)list[i][j];
            }
            off += len;
            written++;
        }
        *(unsigned long *)(page + count_at) = written;
    }

    unsigned long at = (unsigned long)QUARK_PHDRS_AT;
    unsigned long table = QUARK_ARGS_PAGE + at + 16;
    int kept = phnum > QUARK_MAX_PHDRS ? QUARK_MAX_PHDRS : phnum;
    *(unsigned long *)(page + at) = PHDR_SIZE;
    *(unsigned long *)(page + at + 8) = (unsigned long)kept;
    for (int i = 0; i < kept; i++) {
        struct phdr *dst = (struct phdr *)(page + at + 16 + (unsigned long)i * PHDR_SIZE);
        *dst = ph[i];
        if (dst->p_type == PT_PHDR) {
            /* A C library takes the difference between AT_PHDR and this entry
               as the load base, and for a program that is not relocated it has
               to come out zero — for a PIE, where it was put. Left as it was,
               every address derived from the headers would be off by the
               distance to this page. */
            dst->p_vaddr = table - moved;
            dst->p_paddr = table - moved;
        }
    }
}

/* Everything staged, nothing given: throw it away. */
static void drop_stage(const struct segment *segs, int n, unsigned long base,
                       int mapped_stack, int mapped_args) {
    for (int i = 0; i < n; i++) {
        unmap_stage(STAGE_ELF + (segs[i].first - base),
                    (segs[i].end - segs[i].first) / PAGE_SIZE);
    }
    if (mapped_stack) {
        unmap_stage(STAGE_STACK, STACK_PAGES);
    }
    if (mapped_args) {
        unmap_stage(STAGE_ARGS, 1);
    }
}

/* Whether two strings are the same. */
static int same(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

/* Unmap what of an image was staged at `stage`. */
static void unstage(const struct segment *segs, int n, unsigned long base, unsigned long stage) {
    for (int i = 0; i < n; i++) {
        unmap_stage(stage + (segs[i].first - base), (segs[i].end - segs[i].first) / PAGE_SIZE);
    }
}

/* Stage an image read from `fd` at `stage`, laid out as the new program will
   see it from `base`: two segments that share a page write into the same
   staged page. 1, or 0 with nothing left staged. */
static int stage_image(long fd, const struct segment *segs, int n, unsigned long base,
                       unsigned long stage) {
    unsigned long mapped = base;
    for (int i = 0; i < n; i++) {
        unsigned long start = segs[i].first > mapped ? segs[i].first : mapped;
        if (start < segs[i].end &&
            !map_stage(stage + (start - base), (segs[i].end - start) / PAGE_SIZE)) {
            unstage(segs, i, base, stage);
            return 0;
        }
        mapped = segs[i].end;
        /* Fresh pages are zeroed, so the .bss past the file bytes is done. */
        if (segs[i].filesz &&
            __quark_pread(fd, (void *)(stage + (segs[i].vaddr - base)), segs[i].filesz,
                          (long)segs[i].offset) != (long)segs[i].filesz) {
            unstage(segs, i + 1, base, stage);
            return 0;
        }
    }
    return 1;
}

/* Move a staged image into the new address space. */
static int give_image(unsigned long cr3, const struct segment *segs, int n, unsigned long base,
                      unsigned long stage) {
    int ok = 1;
    unsigned long given = base;
    for (int i = 0; i < n && ok; i++) {
        unsigned long start = segs[i].first > given ? segs[i].first : given;
        if (start >= segs[i].end) {
            continue;
        }
        /* A last page the next segment begins in has to suit both, so it goes
           on its own and writable if either wants it. */
        int shared = (i + 1 < n) && segs[i + 1].first < segs[i].end;
        unsigned long whole = shared ? segs[i].end - PAGE_SIZE : segs[i].end;
        if (whole > start) {
            ok = give(cr3, start, stage + (start - base), (whole - start) / PAGE_SIZE, segs[i].writable);
        }
        if (ok && shared) {
            unsigned long last = segs[i].end - PAGE_SIZE;
            int writable = 0;
            for (int j = 0; j < n; j++) {
                if (segs[j].writable && segs[j].first <= last && last < segs[j].end) {
                    writable = 1;
                }
            }
            ok = give(cr3, last, stage + (last - base), 1, writable);
        }
        given = segs[i].end;
    }
    return ok;
}

/* The auxiliary vector's keys, Linux's numbers. */
#define AT_NULL   0
#define AT_PHDR   3
#define AT_PHENT  4
#define AT_PHNUM  5
#define AT_PAGESZ 6
#define AT_BASE   7
#define AT_FLAGS  8
#define AT_ENTRY  9
#define AT_UID    11
#define AT_EUID   12
#define AT_GID    13
#define AT_EGID   14
#define AT_HWCAP  16
#define AT_SECURE 23
#define AT_RANDOM 25
#define AT_HWCAP2 26

/* How long a C string is. */
static unsigned long length(const char *s) {
    unsigned long n = 0;
    while (s[n]) {
        n++;
    }
    return n;
}

/* The most a program's arguments and environment may take where it starts:
   the strings, the pointers to them and the auxiliary vector, at the top of
   its stack. It is musl's ARG_MAX, and what Linux allowed before it let a
   quarter of the stack's limit. More is refused (E2BIG), as Linux refuses
   it, so that the caller can do it another way — rustc writes a linker's
   arguments to a file. It was one page, and what did not fit was left off:
   cargo's rustc started cc with most of its environment gone, and with it
   where the compiler was installed. */
#define ARGS_PAGES 32UL

/* The auxiliary vector's entries, its last nought pair not counted. */
#define NAUX 16UL

/* What `argv` and `envp` take at the top of a stack: their strings, sixteen
   random bytes, and a word for the count, each pointer, each list's nought
   and each auxiliary pair. More than ARGS_PAGES can hold — sixteen to spare
   for where the words begin — is ~0. */
static unsigned long args_size(char *const argv[], char *const envp[]) {
    unsigned long bytes = 16 + (3 + 2 * (NAUX + 1)) * 8;
    for (int section = 0; section < 2; section++) {
        char *const *list = section == 0 ? argv : envp;
        for (unsigned long i = 0; list && list[i]; i++) {
            bytes += length(list[i]) + 1 + 8;
            if (bytes + 16 > ARGS_PAGES * PAGE_SIZE) {
                return ~0UL;
            }
        }
    }
    return bytes;
}

/* The top of the stack, as Linux's kernel leaves it — what a C library's
   entry reads, and a dynamic loader's, before it can call anything. `top`
   is the stack's end in the new program and `staged` where that end is
   here. From `block` up: the number of arguments, a pointer to each, a
   nought, a pointer to each variable of the environment, a nought, and the
   auxiliary vector; above them the strings, and at the top sixteen random
   bytes. `block` is eight below a multiple of sixteen: where the program
   begins with its stack pointer, as a call leaves one, and as
   `quark_rt::spawn` puts it for a program a spawner starts. musl's entry
   aligns it again for itself. As many pages as that takes, args_size
   having said it fits; where it begins is the answer. */
static unsigned long build_stack_top(unsigned char *staged, unsigned long top,
                                     char *const argv[], char *const envp[],
                                     unsigned long phnum, unsigned long entry,
                                     unsigned long interp_base, unsigned long linux) {
    /* The staged address of an address in the new program's stack. */
#define HERE(a) (staged - (top - (a)))
    unsigned long random_at = top - 16;
    __syscall3(SYS_GETRANDOM, (unsigned long)HERE(random_at), 16, 0);
    unsigned long ids = __syscall0(SYS_GET_UID);
    unsigned long uid = ids >> 32, gid = ids & 0xFFFFFFFFUL;
    unsigned int a, b, c, d;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(1), "c"(0));
    /* HWCAP2_FSGSBASE: the program may read and write its own FS and GS
       bases — the processor has the instructions, and the kernel turns them
       on, which it does from 4.5. */
    unsigned int leaves, b7 = 0, x1, x2, x3;
    __asm__ volatile("cpuid" : "=a"(leaves), "=b"(x1), "=c"(x2), "=d"(x3) : "a"(0), "c"(0));
    if (leaves >= 7) {
        __asm__ volatile("cpuid" : "=a"(x1), "=b"(b7), "=c"(x2), "=d"(x3) : "a"(7), "c"(0));
    }
    unsigned long abi = __syscall0(SYS_ABI_VERSION);
    unsigned long hwcap2 = (b7 & 1) && abi >= (4UL << 16 | 5) ? 2 : 0;
    const unsigned long aux[][2] = {
        {phnum ? AT_PHDR : AT_PHNUM, phnum ? QUARK_ARGS_PAGE + QUARK_PHDRS_AT + 16 : 0},
        {AT_PHENT, PHDR_SIZE},
        {AT_PHNUM, phnum},
        {AT_PAGESZ, PAGE_SIZE},
        {AT_BASE, interp_base},
        {AT_FLAGS, 0},
        {AT_ENTRY, entry},
        {AT_UID, uid},
        {AT_EUID, uid},
        {AT_GID, gid},
        {AT_EGID, gid},
        {AT_SECURE, 0},
        {AT_HWCAP, d},
        {AT_HWCAP2, hwcap2},
        {AT_RANDOM, random_at},
        {QUARK_AT_LINUX, linux},
    };
    _Static_assert(sizeof aux / sizeof aux[0] == NAUX, "NAUX is the auxiliary vector's length");

    unsigned long count[2] = {0, 0};
    unsigned long strings = 0;
    for (int section = 0; section < 2; section++) {
        char *const *list = section == 0 ? argv : envp;
        for (unsigned long i = 0; list && list[i]; i++) {
            strings += length(list[i]) + 1;
            count[section]++;
        }
    }
    unsigned long words = 3 + count[0] + count[1] + 2 * (NAUX + 1);
    unsigned long block = ((random_at - strings - words * 8 - 8) & ~15UL) + 8;

    unsigned long *word = (unsigned long *)HERE(block);
    unsigned long w = 0;
    unsigned long str = random_at;
    word[w++] = count[0];
    for (int section = 0; section < 2; section++) {
        char *const *list = section == 0 ? argv : envp;
        for (unsigned long i = 0; i < count[section]; i++) {
            unsigned long len = length(list[i]);
            str -= len + 1;
            unsigned char *at = HERE(str);
            for (unsigned long j = 0; j <= len; j++) {
                at[j] = (unsigned char)list[i][j];
            }
            word[w++] = str;
        }
        word[w++] = 0;
    }
    for (unsigned long i = 0; i < NAUX; i++) {
        word[w++] = aux[i][0];
        word[w++] = aux[i][1];
    }
    word[w++] = AT_NULL;
    word[w++] = 0;
#undef HERE
    return block;
}

/* Tell the kernel what this program is about to become: its arguments, each
   ended by a nought, as much of them as it keeps — what `ps` and `/proc`
   say it is. Said just before it becomes it; a kernel too old to keep it
   says no, and that is all. */
static void say_name(char *const argv[]) {
    char line[128];
    unsigned long n = 0;
    for (unsigned long i = 0; argv && argv[i]; i++) {
        unsigned long len = 0;
        while (argv[i][len]) {
            len++;
        }
        if (n + len + 1 > sizeof line) {
            break;
        }
        for (unsigned long j = 0; j < len; j++) {
            line[n + j] = argv[i][j];
        }
        line[n + len] = 0;
        n += len + 1;
    }
    __syscall4(SYS_PROGRAM_NAME, __syscall0(SYS_GETPID), 0, (unsigned long)line, n);
}

long __quark_execve(const char *path, char *const argv[], char *const envp[]) {
    if (!path || !path[0]) {
        return -LX_ENOENT;
    }
    if (args_size(argv, envp) == ~0UL) {
        return -LX_E2BIG;
    }

    /* The file first, and every check it can fail, before an address space
       exists to leak: `execl` returning an error is an ordinary path, and a
       program that carries on after one must be no worse off for having
       tried. */
    /* Whether it may be run is the server's to say, and asked first: a file
       nobody may execute is refused as that, not found wanting as a program.
       The difference is one a shell acts on — it runs what the kernel calls
       "not a program" as a script. */
    long may = __quark_access(LX_AT_FDCWD, path, 1 /* X_OK */);
    if (may) {
        return may;
    }
    long fd = __quark_open(path, 0 /* O_RDONLY */, 0);
    if (fd < 0) {
        return fd;
    }

    struct ehdr eh;
    long head = __quark_pread(fd, &eh, sizeof eh, 0);
    if (head != (long)sizeof eh) {
        __quark_close(fd);
        /* A directory can be searched and cannot be run. */
        return head == -LX_EISDIR ? -LX_EACCES : -LX_ENOEXEC;
    }
    if (eh.e_ident[0] != 0x7F || eh.e_ident[1] != 'E' || eh.e_ident[2] != 'L' ||
        eh.e_ident[3] != 'F' || eh.e_ident[4] != 2 /* 64-bit */ ||
        eh.e_machine != 62 /* x86-64 */ || eh.e_phentsize < PHDR_SIZE ||
        eh.e_phnum == 0 || eh.e_phnum > QUARK_MAX_PHDRS) {
        __quark_close(fd);
        return -LX_ENOEXEC;
    }

    unsigned long file_size = (unsigned long)__quark_lseek(fd, 0, 2 /* SEEK_END */);

    struct phdr ph[QUARK_MAX_PHDRS];
    for (int i = 0; i < eh.e_phnum; i++) {
        long got = __quark_pread(fd, &ph[i], PHDR_SIZE,
                                 (long)(eh.e_phoff + (unsigned long)i * eh.e_phentsize));
        if (got != (long)PHDR_SIZE) {
            __quark_close(fd);
            return -LX_ENOEXEC;
        }
    }

    /* A program linked to be put anywhere (a PIE) is put somewhere of its
       own, at random, as an interpreter is; one linked to be somewhere is
       put there. */
    unsigned long moved = 0;
    if (eh.e_type == ET_DYN) {
        unsigned long first = ~0UL;
        for (int i = 0; i < eh.e_phnum && first == ~0UL; i++) {
            if (ph[i].p_type == PT_LOAD && ph[i].p_memsz != 0) {
                first = ph[i].p_vaddr & ~(PAGE_SIZE - 1);
            }
        }
        moved = QUARK_PIE_BASE + __quark_random_pages(QUARK_PIE_PAGES) * PAGE_SIZE - first;
    }
    struct segment segs[MAX_SEGMENTS];
    int n = read_segments(ph, eh.e_phnum, file_size, segs, moved);
    if (n < 0) {
        __quark_close(fd);
        return -LX_ENOEXEC;
    }
    unsigned long base = segs[0].first;
    unsigned long entry = eh.e_entry + moved;

    /* A program built to use shared libraries names the loader that finds
       them, and is started in it: a shared object, put at a random base in a
       window of its own (`QUARK_INTERP_BASE`), and told where the program is.
       One that is not there is the program not being runnable, as Linux
       says it: no such file. */
    struct segment isegs[MAX_SEGMENTS];
    int in = 0;
    long ifd = -1;
    unsigned long interp_base = 0, ibase = 0, start_at = entry, linux = 0;
    for (int i = 0; i < eh.e_phnum; i++) {
        if (ph[i].p_type != PT_INTERP) {
            continue;
        }
        char name[256];
        if (ph[i].p_filesz == 0 || ph[i].p_filesz >= sizeof name ||
            __quark_pread(fd, name, ph[i].p_filesz, (long)ph[i].p_offset) != (long)ph[i].p_filesz) {
            __quark_close(fd);
            return -LX_ENOEXEC;
        }
        name[ph[i].p_filesz] = 0;
        /* A program that asks for musl's loader by Linux's name was linked
           for Linux, and its own code may make Linux's calls: it is told
           so (QUARK_AT_LINUX), for its C library — this one, as libc.so —
           to answer them. */
        linux = same(name, QUARK_LINUX_INTERP);
        ifd = __quark_open(name, 0 /* O_RDONLY */, 0);
        if (ifd < 0) {
            __quark_close(fd);
            return ifd;
        }
        struct ehdr ieh;
        struct phdr iph[QUARK_MAX_PHDRS];
        int good = __quark_pread(ifd, &ieh, sizeof ieh, 0) == (long)sizeof ieh &&
                   ieh.e_ident[0] == 0x7F && ieh.e_ident[1] == 'E' && ieh.e_ident[2] == 'L' &&
                   ieh.e_ident[3] == 'F' && ieh.e_type == ET_DYN && ieh.e_machine == 62 &&
                   ieh.e_phentsize >= PHDR_SIZE && ieh.e_phnum > 0 && ieh.e_phnum <= QUARK_MAX_PHDRS;
        for (int j = 0; good && j < ieh.e_phnum; j++) {
            good = __quark_pread(ifd, &iph[j], PHDR_SIZE,
                                 (long)(ieh.e_phoff + (unsigned long)j * ieh.e_phentsize)) == (long)PHDR_SIZE;
        }
        if (good) {
            interp_base = QUARK_INTERP_BASE + __quark_random_pages(QUARK_INTERP_PAGES) * PAGE_SIZE;
            unsigned long isize = (unsigned long)__quark_lseek(ifd, 0, 2 /* SEEK_END */);
            in = read_segments(iph, ieh.e_phnum, isize, isegs, interp_base);
            good = in > 0;
        }
        if (!good) {
            __quark_close(ifd);
            __quark_close(fd);
            return -LX_ELIBBAD;
        }
        ibase = isegs[0].first;
        start_at = interp_base + ieh.e_entry;
        break;
    }

    /* Stage the image where the program will see it, laid out as it will see
       it, and the interpreter's beside it. */
    if (!stage_image(fd, segs, n, base, STAGE_ELF)) {
        __quark_close(fd);
        if (ifd >= 0) {
            __quark_close(ifd);
        }
        return -LX_ENOMEM;
    }
    __quark_close(fd);
    if (ifd >= 0) {
        int staged = stage_image(ifd, isegs, in, ibase, STAGE_INTERP);
        __quark_close(ifd);
        if (!staged) {
            unstage(segs, n, base, STAGE_ELF);
            return -LX_ENOMEM;
        }
    }

    /* Where the new program's stack ends: a random number of pages into the
       two gigabytes below the highest it may reach. Its top page is what a C
       program finds there when it starts, and its stack pointer is at the
       bottom of that page. */
    unsigned long stack_top = QUARK_STACK_TOP - __quark_random_pages(1UL << 19) * PAGE_SIZE;
    if (!map_stage(STAGE_STACK, STACK_PAGES)) {
        unstage(isegs, in, ibase, STAGE_INTERP);
        drop_stage(segs, n, base, 0, 0);
        return -LX_ENOMEM;
    }
    if (!map_stage(STAGE_ARGS, 1)) {
        unstage(isegs, in, ibase, STAGE_INTERP);
        drop_stage(segs, n, base, 1, 0);
        return -LX_ENOMEM;
    }
    build_args((unsigned char *)STAGE_ARGS, argv, envp, ph, eh.e_phnum, moved);
    unsigned long block = build_stack_top((unsigned char *)(STAGE_STACK + STACK_PAGES * PAGE_SIZE),
                                          stack_top, argv, envp, (unsigned long)eh.e_phnum, entry,
                                          interp_base, linux);

    unsigned long cr3 = __syscall0(SYS_ADDRSPACE_CREATE);
    if (cr3 == QUARK_ERR) {
        unstage(isegs, in, ibase, STAGE_INTERP);
        drop_stage(segs, n, base, 1, 1);
        return -LX_ENOMEM;
    }

    int ok = give_image(cr3, segs, n, base, STAGE_ELF);
    if (ok && in > 0) {
        ok = give_image(cr3, isegs, in, ibase, STAGE_INTERP);
    }
    if (ok) {
        ok = give(cr3, stack_top - STACK_PAGES * PAGE_SIZE, STAGE_STACK, STACK_PAGES, 1);
    }
    if (ok) {
        ok = give(cr3, QUARK_ARGS_PAGE, STAGE_ARGS, 1, 0);
    }
    if (!ok) {
        /* Whatever was given belongs to the new space, and destroying it frees
           exactly that; whatever was not is still ours to unmap. */
        __syscall1(SYS_ADDRSPACE_DESTROY, cr3);
        unstage(isegs, in, ibase, STAGE_INTERP);
        drop_stage(segs, n, base, 1, 1);
        return -LX_ENOMEM;
    }

    /* The last call this program makes. Everything above it was preparation
       that could fail and leave the caller as it was; this does not return. */
    say_name(argv);
    __syscall3(SYS_EXEC_SPACE, cr3, start_at, block);
    /* Only reached if the kernel refused, which means the space is not the
       caller's or has a task in it — neither of which can be true here. */
    __syscall1(SYS_ADDRSPACE_DESTROY, cr3);
    return -LX_ENOEXEC;
}
