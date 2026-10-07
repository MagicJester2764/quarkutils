/* Who a program is, and how it stops being root.
 *
 * A task here has one user, one group and the groups it is in besides, and
 * what lets a program change them is a capability rather than being user 0.
 * For a long time the C library answered "one group, your own" to anybody
 * who asked and "yes" to anybody who set one; and a program that was root
 * and made itself somebody else still held what would make it root again —
 * which is the one thing `setuid` promises it cannot be.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#include <quark/manifest.h>
#include <quark/syscall.h>

/* What lets a program say who it is. It is given only what it asks for, and
   only what whoever starts it holds: started by a user, this gets nothing. */
QUARK_MANIFEST(QUARK_CAP_SET_UID, 0UL, 0UL);

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

/* Run `body` in a child and say what it exited with. */
static int in_a_child(int (*body)(void)) {
    pid_t pid = fork();
    if (pid == 0) {
        _exit(body());
    }
    int status = 0;
    if (pid < 0 || waitpid(pid, &status, 0) != pid || !WIFEXITED(status)) {
        return -1;
    }
    return WEXITSTATUS(status);
}

/* A forked child is who its parent was. */
static int in_three(void) {
    gid_t in[4];
    return getgroups(4, in) == 3 && in[0] == 7 && in[1] == 9 && in[2] == 11 ? 0 : 1;
}

/* Root, becoming somebody for good. A bit for each thing that is then so. */
static int for_good(void) {
    gid_t one = 50, in[4];
    int ok = 0;
    if (setgroups(1, &one) == 0 && setgid(60) == 0 && setuid(70) == 0) {
        ok |= 1;
    }
    if (getuid() == 70 && geteuid() == 70 && getgid() == 60 && getgroups(4, in) == 1 && in[0] == 50) {
        ok |= 2;
    }
    /* No way back, by any of the names it has. */
    if (setuid(0) == -1 && errno == EPERM && seteuid(0) == -1 && setreuid(0, 0) == -1 && getuid() == 70) {
        ok |= 4;
    }
    /* Nor to another group, nor out of the ones it is in. */
    if (setgid(0) == -1 && errno == EPERM && setgroups(0, NULL) == -1 && errno == EPERM) {
        ok |= 8;
    }
    /* And it is that user to the file server. */
    if (open("/etc/shadow", O_RDONLY) == -1 && errno == EACCES) {
        ok |= 16;
    }
    /* Saying what is already so is always allowed. */
    if (setuid(70) == 0 && setgid(60) == 0 && setgroups(1, &one) == 0) {
        ok |= 32;
    }
    return ok;
}

/* Root, holding the right to say who it is far up its capabilities as well
   as where it was given it: becoming somebody for good gives up every one.
   The C library looked through the first sixty-four slots, and a space has
   had 256 since, and grows now. A bit for each thing that is then so. */
static int kept_far_up(void) {
    static const unsigned long far[2] = { 200, 2000 };
    unsigned long me = __syscall0(SYS_GETPID);
    int ok = 0, minted = 0, left = 0;
    for (int i = 0; i < 2; i++) {
        minted += __syscall4(SYS_CAP_MINT, far[i], QUARK_CAP_TYPE_SET_UID, 0, 0) == 0;
    }
    if (minted == 2) {
        ok |= 1;
    }
    if (setuid(70) == 0) {
        for (int i = 0; i < 2; i++) {
            unsigned long cap[4];
            left += __syscall3(SYS_CAP_READ, me, far[i], (unsigned long)cap) != QUARK_ERR &&
                    cap[0] == QUARK_CAP_TYPE_SET_UID;
        }
        if (left == 0) {
            ok |= 2;
        }
    }
    if (setuid(0) == -1 && errno == EPERM) {
        ok |= 4;
    }
    return ok;
}

/* Root, becoming somebody for a while: `seteuid` leaves the way back. */
static int for_a_while(void) {
    int ok = 0;
    if (seteuid(70) == 0 && geteuid() == 70) {
        ok |= 1;
    }
    if (open("/etc/shadow", O_RDONLY) == -1 && errno == EACCES) {
        ok |= 2;
    }
    if (seteuid(0) == 0 && geteuid() == 0) {
        ok |= 4;
    }
    int fd = open("/etc/shadow", O_RDONLY);
    if (fd >= 0) {
        ok |= 8;
        close(fd);
    }
    /* And then for good, by the form that names all three. */
    if (setresuid(71, 71, 71) == 0 && getuid() == 71 && setuid(0) == -1 && seteuid(0) == -1) {
        ok |= 16;
    }
    return ok;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("who a program is:\n");
    gid_t was[32], in[32];
    int had = getgroups(32, was);
    check("a program says which groups it is in", had >= 0 && had <= 16);
    check("and how many, asked with no room", getgroups(0, NULL) == had);
    check("the real and the effective user are one", getuid() == geteuid() && getgid() == getegid());
    check("asking to be who it is, is always allowed", setuid(getuid()) == 0 && setgid(getgid()) == 0);
    check("and to be in the groups it is in", setgroups((size_t)had, was) == 0);

    gid_t three[3] = { 7, 9, 11 };
    if (getuid() != 0 || setgroups(3, three) != 0) {
        /* A user, or root started by something that does not hold the
           right: what is left is that the answer is no. */
        check("without the right to, a program is not put in other groups",
              setgroups(3, three) == -1 && errno == EPERM);
        check("nor made another user", setuid(getuid() + 1) == -1 && errno == EPERM);
        printf("  (the rest needs root, and the right to say who a task is)\n");
        return failed ? 1 : 0;
    }
    check("root puts itself in three groups", getgroups(32, in) == 3 && !memcmp(in, three, sizeof three));
    check("too little room for them is an error", getgroups(2, in) == -1 && errno == EINVAL);
    gid_t many[17] = { 0 };
    check("seventeen is one too many", setgroups(17, many) == -1 && errno == EINVAL);
    check("a child is in them too", in_a_child(in_three) == 0);

    check("root becomes somebody, in a group and in groups besides, for good", in_a_child(for_good) == 63);
    check("and gives up the right to say who it is wherever it held it", in_a_child(kept_far_up) == 7);
    check("or for a while, and comes back, and then for good", in_a_child(for_a_while) == 31);
    check("and is still root itself", getuid() == 0 && setgroups((size_t)had, was) == 0);
    return failed ? 1 : 0;
}
