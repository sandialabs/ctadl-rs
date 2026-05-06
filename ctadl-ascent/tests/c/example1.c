/* example.c – simple data‑flow chain:
 *   produce() returns a value → middle() receives it (upward flow)
 *   middle() passes the value to consume() (downward flow)
 */

#include <stdio.h>

/* Source of data */
int produce(void) {
    int v = 42;          /* <- value originates here */
    return v;
}

/* Sink that uses the data */
void consume(int x) {
    printf("Consumed: %d\n", x);
}

/* Intermediate function that links the two */
void middle(void) {
    int x = produce();   /* upward flow: return of produce() → local y */
    int y;
    y = x;
    consume(y);          /* downward flow: y passed to consume() */
}

/* Entry point */
int main(void) {
    middle();
    return 0;
}

