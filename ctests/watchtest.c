/* A program that has gone is known to have gone.
 *
 * A server keeps things for the programs that call it — what they have
 * open, the locks they hold, where they are — and lets go of them when the
 * kernel says a program has gone (SYS_SPACE_WATCH). Whatever the kernel
 * does not say, a server keeps for ever, and three things it did not say:
 *
 * - A program that became another. `exec` moves a task into a new address
 *   space, which is a new program to every server; nobody was told the old
 *   one was no more. What it held as a program was held until the machine
 *   was turned off, and the kernel went on counting it as watched: after a
 *   hundred and twenty-eight commands it could watch no more, and from
 *   then on no server heard of any program ending at all.
 * - The ninth of nine. What a watcher had not collected yet was kept in a
 *   list eight long, and one call can end more programs than that: every
 *   member of a pipeline, when it is interrupted.
 * - And a file server found out the second way. A removed directory that a
 *   killed program had been looking at was never freed.
 *
 * This asks the kernel itself, as a server would, and then looks at what a
 * file server made of it.
 */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#include <quark/syscall.h>

#define SELF "/usr/bin/watchtest"

/* From the kernel, sender 0: a task that was watched has died (its id in
   data[0]), and a program that was watched has no task left (its id). */
#define TAG_TASK_DIED  0xFFFF0003UL
#define TAG_SPACE_DIED 0xFFFF0004UL

/* Where a program that is to stay finds the pipe it waits on. */
#define HOLD_FD 9

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* The task a process is: the one whose process id is `pid`. A process that
   has made no threads is one task. */
static unsigned long tid_of(pid_t pid) {
    for (unsigned long tid = 1; (tid = __syscall1(SYS_TASK_NEXT, tid)) != QUARK_ERR; tid++) {
        if (__syscall1(SYS_PID, tid) == (unsigned long)pid) {
            return tid;
        }
    }
    return 0;
}

static unsigned long space_of(unsigned long tid) {
    unsigned long space = __syscall1(SYS_TASK_SPACE, tid);
    return space == QUARK_ERR ? 0 : space;
}

/* The next thing the kernel has to say to this task, within `ticks`
   hundredths of a second. 0 if it says nothing. */
static int told(struct quark_msg *msg, unsigned long ticks) {
    memset(msg, 0, sizeof *msg);
    return __syscall3(SYS_RECV_TIMEOUT, 0, (unsigned long)msg, ticks) == 0 && msg->sender == 0;
}

/* Whether the kernel says program `space` has gone, within three seconds. */
static int told_gone(unsigned long space) {
    struct quark_msg msg;
    while (told(&msg, 300)) {
        if (msg.tag == TAG_SPACE_DIED && msg.data[0] == space) {
            return 1;
        }
    }
    return 0;
}

/* A child that waits to be told to go on, and then does `then`. */
static pid_t child_that(int go[2], int hold[2], const char *then) {
    pid_t pid = fork();
    if (pid != 0) {
        return pid;
    }
    char c;
    close(go[1]);
    if (read(go[0], &c, 1) != 1) {
        _exit(90);
    }
    if (!then) {
        _exit(0);
    }
    if (hold) {
        close(hold[1]);
        dup2(hold[0], HOLD_FD);
    }
    execl(SELF, SELF, then, (char *)NULL);
    _exit(91);
}

static int exited_with(pid_t pid, int code) {
    int st = 0;
    return waitpid(pid, &st, 0) == pid && WIFEXITED(st) && WEXITSTATUS(st) == code;
}

#define MANY 12

int main(int argc, char **argv) {
    /* What a child becomes: a program that ends at once, or one that waits
       to be told to. */
    if (argc > 1 && !strcmp(argv[1], "--quit")) {
        _exit(0);
    }
    if (argc > 1 && !strcmp(argv[1], "--stay")) {
        char c;
        _exit(read(HOLD_FD, &c, 1) == 1 ? 0 : 92);
    }

    setvbuf(stdout, NULL, _IONBF, 0);
    printf("a program that has gone:\n");

    int go[2];
    if (pipe(go)) {
        printf("watchtest: no pipe\n");
        return 1;
    }

    /* One that ends. */
    pid_t pid = child_that(go, NULL, NULL);
    unsigned long space = space_of(tid_of(pid));
    check("a program is watched", pid > 0 && space != 0 && __syscall1(SYS_SPACE_WATCH, space) == 0);
    write(go[1], "x", 1);
    check("and when it ends, whoever watched is told", told_gone(space));
    check("it ended as it meant to", exited_with(pid, 0));

    /* One that becomes another program, and is still running. */
    int hold[2];
    pipe(hold);
    pid = child_that(go, hold, "--stay");
    unsigned long tid = tid_of(pid);
    space = space_of(tid);
    int watched = pid > 0 && space != 0 && __syscall1(SYS_SPACE_WATCH, space) == 0;
    write(go[1], "x", 1);
    check("a program that becomes another has gone, and whoever watched is told",
          watched && told_gone(space));
    int st = 0;
    unsigned long became = space_of(tid);
    check("the process goes on, as another program",
          waitpid(pid, &st, WNOHANG) == 0 && became != 0 && became != space);
    watched = became != 0 && __syscall1(SYS_SPACE_WATCH, became) == 0;
    write(hold[1], "x", 1);
    check("which is watched in its turn, and told of when it ends", watched && told_gone(became));
    check("and that is the end of the process", exited_with(pid, 0));

    /* More of them than the kernel has room to watch at once. */
    int each = 0;
    for (int i = 0; i < 150; i++) {
        pid = child_that(go, NULL, "--quit");
        space = space_of(tid_of(pid));
        watched = pid > 0 && space != 0 && __syscall1(SYS_SPACE_WATCH, space) == 0;
        write(go[1], "x", 1);
        int heard = watched && told_gone(space);
        if (pid > 0) {
            waitpid(pid, &st, 0);
        }
        if (!heard) {
            break;
        }
        each++;
    }
    printf("        %d programs watched, one after another\n", each);
    check("a hundred and fifty programs that become others are each watched, and each told of",
          each == 150);

    /* A lock is the program's, and a file server lets it go when it hears
       the program has gone. Taken before an exec, it used to be nobody's
       and never let go. */
    char name[64];
    snprintf(name, sizeof name, "/tmp/watchtest.%d", (int)getpid());
    int fd = open(name, O_RDWR | O_CREAT | O_TRUNC, 0600);
    struct flock whole = { .l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0 };
    pid = fork();
    if (pid == 0) {
        int mine = open(name, O_RDWR);
        if (mine < 0 || fcntl(mine, F_SETLK, &whole) != 0) {
            _exit(93);
        }
        execl(SELF, SELF, "--quit", (char *)NULL);
        _exit(91);
    }
    int ended = pid > 0 && exited_with(pid, 0);
    struct flock probe = whole;
    check("a lock a program took before it became another does not outlast the process",
          fd >= 0 && ended && fcntl(fd, F_GETLK, &probe) == 0 && probe.l_type == F_UNLCK);
    if (fd >= 0) {
        close(fd);
    }
    unlink(name);

    /* Twelve programs ended by one call, each of them watched twice over:
       as a program, and as a task. */
    pid_t group = 0;
    pid_t pids[MANY] = { 0 };
    unsigned long spaces[MANY] = { 0 }, tids[MANY] = { 0 };
    int ready[2];
    pipe(ready);
    int started = 0;
    for (int i = 0; i < MANY; i++) {
        pids[i] = fork();
        if (pids[i] == 0) {
            setpgid(0, group);
            write(ready[1], "r", 1);
            for (;;) {
                pause();
            }
        }
        if (pids[i] < 0) {
            break;
        }
        if (i == 0) {
            group = pids[0];
        }
        setpgid(pids[i], group);
        char c;
        if (read(ready[0], &c, 1) != 1) {
            break;
        }
        tids[i] = tid_of(pids[i]);
        spaces[i] = space_of(tids[i]);
        if (spaces[i] == 0 || __syscall1(SYS_SPACE_WATCH, spaces[i]) != 0 ||
            __syscall1(SYS_TASK_WATCH, tids[i]) != 0) {
            break;
        }
        started++;
    }
    check("twelve programs are started, and each is watched", started == MANY);
    int gone_programs = 0, gone_tasks = 0;
    if (started > 0 && kill(-group, SIGKILL) == 0) {
        struct quark_msg msg;
        int heard_space[MANY] = { 0 }, heard_task[MANY] = { 0 };
        while (gone_programs + gone_tasks < 2 * started && told(&msg, 300)) {
            for (int i = 0; i < started; i++) {
                if (msg.tag == TAG_SPACE_DIED && msg.data[0] == spaces[i] && !heard_space[i]) {
                    heard_space[i] = 1;
                    gone_programs++;
                }
                if (msg.tag == TAG_TASK_DIED && msg.data[0] == tids[i] && !heard_task[i]) {
                    heard_task[i] = 1;
                    gone_tasks++;
                }
            }
        }
    }
    for (int i = 0; i < MANY; i++) {
        if (pids[i] > 0) {
            if (i >= started) {
                kill(pids[i], SIGKILL);
            }
            waitpid(pids[i], &st, 0);
        }
    }
    printf("        %d programs and %d tasks told of\n", gone_programs, gone_tasks);
    check("ended at one stroke, every program of them is told of", gone_programs == MANY);
    check("and every task", gone_tasks == MANY);

    if (failed) {
        printf("watchtest: FAILED\n");
        return 1;
    }
    printf("watchtest: ok\n");
    return 0;
}
