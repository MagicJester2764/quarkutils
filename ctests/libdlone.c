/* A shared library a program opens for itself (`dltest`): a function, a
 * datum, a constructor that runs when it is opened, a thread-local of its
 * own, and errno through the C library they share.
 */
#include <errno.h>
#include <unistd.h>

int dlone_constructed;
int dlone_value = 1234;
__thread int dlone_tls = 7;

__attribute__((constructor)) static void made(void)
{
    dlone_constructed = 42;
}

int dlone_add(int a, int b)
{
    return a + b;
}

int dlone_tls_get(void)
{
    return dlone_tls;
}

void dlone_tls_set(int v)
{
    dlone_tls = v;
}

/* What a failed call says, said through the program's C library. */
int dlone_errno(void)
{
    errno = 0;
    close(-1);
    return errno;
}
