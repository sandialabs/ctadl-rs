/*
 * Constants of one value and different types. Ghidra names an unnamed constant after its
 * value (`const:00000000`), so these are distinct high variables of `pick` with one name:
 * `NULL` is a `char *`, the `0`s a `long`, an `int` and a `bool`. Each must keep its own id in
 * the exported facts, and so its own type.
 */
#include <stddef.h>

char *pick(char *buf, long n)
{
    char *p = NULL;
    long count = 0;
    int flag = 0;
    for (long i = 0; i < n; i++) {
        if (buf[i] == 'x') {
            p = buf + i;
            count++;
            flag = 1;
        }
    }
    if (flag != 0 && count > 0)
        return p;
    return NULL;
}
