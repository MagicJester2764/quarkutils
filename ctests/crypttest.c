/* Passwords, and the files accounts are kept in, as the C library reads
 * them.
 *
 * The system's own programs hash a password with code of their own — nothing
 * that is not the C library can call `crypt` — and write the account files
 * themselves. Both have to be what every C program expects: a hash `passwd`
 * wrote is one `crypt` must verify, and an `/etc/passwd` in any other shape
 * is one `getpwnam` reads nobody out of. `dtest` holds the system's code to
 * the same three hashes.
 */
#define _GNU_SOURCE
#include <crypt.h>
#include <grp.h>
#include <pwd.h>
#include <shadow.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int failed;

static void check(const char *what, int ok) {
    printf("  %s  %s\n", ok ? "ok  " : "FAIL", what);
    if (!ok) {
        failed++;
    }
}

static int hashes(const char *password, const char *setting, const char *want) {
    const char *got = crypt(password, setting);
    return got && !strcmp(got, want);
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("passwords and accounts:\n");
    check("a password hashes as it does on any Unix",
          hashes("Hello world!", "$6$saltstring",
                 "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"));
    check("with the rounds it is told, and sixteen characters of salt",
          hashes("Hello world!", "$6$rounds=10000$saltstringsaltstring",
                 "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v."));
    check("a password longer than a block of the hash",
          hashes("a very much longer text to encrypt.  This one even stretches over morethan one line.",
                 "$6$rounds=1400$anotherlongsaltstring",
                 "$6$rounds=1400$anotherlongsalts$POfYwTEok97VWcjxIiSOjiykti.o/pQs.wPvMxQ6Fm7I6IoYN3CmLs66x9t0oSwbtEW7o7UmJEiDwGqd8p4ur1"));

    struct passwd *root = getpwnam("root");
    check("root is found by name", root && root->pw_uid == 0 && root->pw_gid == 0);
    check("with a home and a shell", root && root->pw_dir[0] == '/' && root->pw_shell[0] == '/');
    struct passwd *zero = getpwuid(0);
    check("and by number", zero && !strcmp(zero->pw_name, "root"));
    check("somebody who is not there is not found", getpwnam("nobody-of-this-name") == NULL);
    struct group *group = getgrgid(0);
    check("its group is found", group && group->gr_gid == 0 && group->gr_name[0]);

    /* The passwords are root's to read. */
    struct spwd *shadow = getspnam("root");
    if (getuid() == 0) {
        check("root reads its own line of the passwords", shadow && !strcmp(shadow->sp_namp, "root"));
        /* Whatever the hash is, it is one of the three things a hash can
           be: nothing, locked, or one `crypt` makes again from itself. */
        const char *hash = shadow ? shadow->sp_pwdp : "!";
        const char *again = hash[0] == '$' ? crypt("", hash) : NULL;
        check("and what is there is nothing, a lock, or a hash this library knows",
              hash[0] == '\0' || hash[0] == '!' || hash[0] == '*' || (again && again[0] == '$'));
    } else {
        check("the passwords are not a user's to read", shadow == NULL);
    }
    return failed ? 1 : 0;
}
