/* What a process is called: a number that is not somebody else's a moment
 * later.
 *
 * Quark gives a dead task's id to the next task made, and for a while that
 * was a C program's pid too. Everything a shell does with a child's number
 * assumes otherwise. bash, with job control off, does not wait for a command
 * whose pid is the last background job's; after `sleep 2 &` that was every
 * command that landed in the slot, and they ran with the prompt already
 * back. And `kill` of a number remembered from earlier killed whatever had
 * it now. */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define SELF "/usr/bin/pidtest"

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static pid_t thread_pid, thread_tid;

static void *in_a_thread(void *arg) {
    (void)arg;
    thread_pid = getpid();
    thread_tid = (pid_t)syscall(SYS_gettid);
    return NULL;
}

int main(int argc, char **argv) {
    /* The far side of an exec: the same process, so the same number. */
    if (argc == 3 && !strcmp(argv[1], "after-exec")) {
        return getpid() == atoi(argv[2]) ? 0 : 1;
    }

    printf("process ids:\n");
    pid_t self = getpid();
    int status = 0;

    /* One child after another: each is collected before the next is made, so
       each is given the task the last one had. */
    pid_t seen[4];
    int distinct = 1, collected = 1;
    for (int i = 0; i < 4; i++) {
        seen[i] = fork();
        if (seen[i] == 0) {
            _exit(10 + i);
        }
        if (waitpid(seen[i], &status, 0) != seen[i] || WEXITSTATUS(status) != 10 + i) {
            collected = 0;
        }
        for (int j = 0; j < i; j++) {
            if (seen[j] == seen[i]) {
                distinct = 0;
            }
        }
    }
    check("each child is waited for by the number fork gave", collected);
    check("and no two of them had the same one", distinct);

    /* What a shell does: something in the background, then something else,
       and the second is waited for by its own number. */
    pid_t slow = fork();
    if (slow == 0) {
        usleep(200 * 1000);
        _exit(7);
    }
    pid_t quick = fork();
    if (quick == 0) {
        _exit(3);
    }
    check("a second child is not the first", quick != slow);
    check("the second is collected by its number",
          waitpid(quick, &status, 0) == quick && WEXITSTATUS(status) == 3);
    check("and then the first by its own",
          waitpid(slow, &status, 0) == slow && WEXITSTATUS(status) == 7);

    /* A number remembered from earlier names nothing now, and in particular
       not whoever has the task. */
    pid_t gone = seen[3];
    pid_t bystander = fork();
    if (bystander == 0) {
        for (;;) {
            sleep(10);
        }
    }
    errno = 0;
    check("a signal for a child that was collected finds nobody",
          kill(gone, SIGTERM) == -1 && errno == ESRCH);
    check("and the child that came after it is still running",
          waitpid(bystander, &status, WNOHANG) == 0);
    check("which a signal by its own number ends",
          kill(bystander, SIGTERM) == 0 && waitpid(bystander, &status, 0) == bystander
              && WIFSIGNALED(status) && WTERMSIG(status) == SIGTERM);
    errno = 0;
    check("waiting for a number that is nobody's child is refused",
          waitpid(gone, &status, 0) == -1 && errno == ECHILD);

    /* A child that has ended and not been collected is still there to name. */
    pid_t zombie = fork();
    if (zombie == 0) {
        _exit(0);
    }
    usleep(100 * 1000);
    check("a signal for a child not yet collected is accepted", kill(zombie, 0) == 0);
    check("and it is still there to collect", waitpid(zombie, &status, 0) == zombie);

    /* Who a child says its parent is. */
    pid_t child = fork();
    if (child == 0) {
        _exit(getppid() == self ? 0 : 1);
    }
    check("a child's parent is the number its parent has",
          waitpid(child, &status, 0) == child && WEXITSTATUS(status) == 0);

    /* exec changes the program and not the process. */
    child = fork();
    if (child == 0) {
        char number[16];
        snprintf(number, sizeof number, "%d", (int)getpid());
        execl(SELF, "pidtest", "after-exec", number, (char *)NULL);
        _exit(2);
    }
    check("a process keeps its number through exec",
          waitpid(child, &status, 0) == child && WEXITSTATUS(status) == 0);

    /* A thread is in its process, and is a thread of its own. */
    pthread_t t;
    pid_t main_tid = (pid_t)syscall(SYS_gettid);
    int made = pthread_create(&t, NULL, in_a_thread, NULL) == 0 && pthread_join(t, NULL) == 0;
    check("a thread has its process's id", made && thread_pid == self);
    check("and a thread id of its own", made && thread_tid > 0 && thread_tid != main_tid);
    check("and none of this changed who this is", getpid() == self);

    printf("pidtest: %s\n", failed ? "FAILED" : "ok");
    return failed ? 1 : 0;
}
