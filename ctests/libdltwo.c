/* A shared library a program is linked to (`dltest`): loaded before the
 * program runs, because the program names it.
 */
int dltwo_calls;

int dltwo_twice(int x)
{
    dltwo_calls++;
    return 2 * x;
}
