/* What a program is told about the processors it runs on.
 *
 * sched_getcpu names an online processor. /proc/stat has a line for every
 * processor, as Linux's does — the time each has spent in programs, in the
 * kernel, with nothing to do and taking interrupts, in hundredths of a
 * second — and the machine's lines after them; and a processor with
 * nothing to do is counted as such: a second's sleep adds to it, on every
 * processor. One that had run nothing since it was started was counted as
 * in the kernel for as long as it slept, and the sum hid it. The
 * second number of /proc/uptime is that idle time too, where it was always
 * 0.00. /proc/cpuinfo has a block for each processor with its package,
 * core and APIC id as the processor says them, and /proc/self/stat's
 * thirty-ninth field the processor the program last ran on.
 *
 * With an argument, `cpuinfo packages N`, there are N packages: the machine
 * started with `-smp 4,sockets=2,cores=2,threads=1` has two.
 *
 * Exits 0 only if every check holds.
 */
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed = 1;
    }
}

#define MOST 256

/* The idle hundredths of every `cpuN` line of /proc/stat, summed, and how
   many such lines there were; -1 if there is no /proc/stat. Each line's own
   goes in `each`, by its processor's number. */
static long idle_of_all(int *lines, int *machine, long *each) {
    FILE *f = fopen("/proc/stat", "r");
    if (!f) {
        return -1;
    }
    char line[512];
    long idle = 0;
    *lines = 0;
    *machine = 0;
    while (fgets(line, sizeof line, f)) {
        unsigned long long user, nice, sys, quiet;
        int n;
        /* `cpuN`, not the machine's `cpu` line before them: %d would skip
           the spaces after that one and read its first column. */
        int numbered = strncmp(line, "cpu", 3) == 0 && line[3] >= '0' && line[3] <= '9';
        if (numbered && sscanf(line, "cpu%d %llu %llu %llu %llu", &n, &user, &nice, &sys, &quiet) == 5) {
            idle += (long)quiet;
            (*lines)++;
            if (n >= 0 && n < MOST) {
                each[n] = (long)quiet;
            }
        }
        static const char *const wanted[] = {"intr ", "ctxt ", "btime ", "processes ", "procs_running "};
        for (unsigned i = 0; i < sizeof wanted / sizeof wanted[0]; i++) {
            if (strncmp(line, wanted[i], strlen(wanted[i])) == 0) {
                *machine |= 1 << i;
            }
        }
    }
    fclose(f);
    return idle;
}

/* How many distinct `physical id` values /proc/cpuinfo has, how many
   processor blocks, and how many of those name an APIC id. */
static void cpuinfo(int *packages, int *blocks, int *apic) {
    *packages = *blocks = *apic = 0;
    FILE *f = fopen("/proc/cpuinfo", "r");
    if (!f) {
        return;
    }
    char line[512];
    int seen[256] = {0};
    while (fgets(line, sizeof line, f)) {
        int v;
        if (sscanf(line, "processor : %d", &v) == 1 || sscanf(line, "processor\t: %d", &v) == 1) {
            (*blocks)++;
        } else if (sscanf(line, "physical id\t: %d", &v) == 1 && v >= 0 && v < 256 && !seen[v]) {
            seen[v] = 1;
            (*packages)++;
        } else if (sscanf(line, "apicid\t\t: %d", &v) == 1) {
            (*apic)++;
        }
    }
    fclose(f);
}

/* The thirty-ninth field of /proc/self/stat: the processor this program
   last ran on. -1 if it cannot be read. */
static long last_processor(void) {
    FILE *f = fopen("/proc/self/stat", "r");
    if (!f) {
        return -1;
    }
    char line[1024];
    char *ok = fgets(line, sizeof line, f);
    fclose(f);
    char *p = ok ? strrchr(line, ')') : NULL;
    if (!p) {
        return -1;
    }
    /* After the name: field 3, the state, and on to 39. */
    p++;
    for (int field = 3; field < 39; field++) {
        p = strchr(p + 1, ' ');
        if (!p) {
            return -1;
        }
    }
    return strtol(p + 1, NULL, 10);
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("cpuinfo:\n");
    long online = sysconf(_SC_NPROCESSORS_ONLN);
    int cpu = sched_getcpu();
    check("sched_getcpu names an online processor", online >= 1 && cpu >= 0 && cpu < online);

    int lines = 0, machine = 0;
    static long each_before[MOST], each_after[MOST];
    long before = idle_of_all(&lines, &machine, each_before);
    check("/proc/stat has a line for every processor", before >= 0 && lines == online);
    check("and the machine's: intr, ctxt, btime, processes, procs_running", machine == 0x1f);
    struct timespec second = {1, 0};
    nanosleep(&second, NULL);
    long after = idle_of_all(&lines, &machine, each_after);
    /* A second asleep on an otherwise quiet machine is a hundred hundredths
       a processor; half of one is plenty to tell it from nothing. */
    check("a second's sleep is counted as time with nothing to do", before >= 0 && after - before >= 50);
    /* And on each processor: a quarter of the second at least, every one. */
    int each_quiet = before >= 0;
    for (int i = 0; i < online && i < MOST; i++) {
        long quiet = each_after[i] - each_before[i];
        if (quiet < 25) {
            printf("  (processor %d: %ld hundredths with nothing to do)\n", i, quiet);
            each_quiet = 0;
        }
    }
    check("on every processor", each_quiet);

    double up = 0, idle = 0;
    FILE *f = fopen("/proc/uptime", "r");
    int read = f ? fscanf(f, "%lf %lf", &up, &idle) : 0;
    if (f) {
        fclose(f);
    }
    check("/proc/uptime says how long the processors have had nothing to do", read == 2 && idle > 0.4);

    int packages, blocks, apic;
    cpuinfo(&packages, &blocks, &apic);
    check("/proc/cpuinfo has a block for every processor, each with its APIC id",
          blocks == online && apic == online);
    if (argc > 2 && strcmp(argv[1], "packages") == 0) {
        int want = atoi(argv[2]);
        printf("  (%d packages said, %d wanted)\n", packages, want);
        check("and as many packages as the machine was started with", packages == want);
    } else {
        check("and a package for each", packages >= 1);
    }

    long last = last_processor();
    check("/proc/self/stat says an online processor it last ran on", last >= 0 && last < online);

    printf("cpuinfo: %s\n", failed ? "FAILED" : "passed");
    return failed;
}
