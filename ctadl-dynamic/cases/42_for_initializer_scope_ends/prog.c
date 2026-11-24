/* Spec 051: a `for` initializer's scope ends with the loop. `int i = source(); for (int i = 0; i < 2; i++) { .. } sink(i)` reads the outer `i`. Before, every name after the loop resolved in the loop's scope, so the sink read the loop's `i`. s=flow d=flow. */
int source();
void sink(int);

int main(void) { int i = source(); for (int i = 0; i < 2; i++) { sink(0); } sink(i); return 0; }
