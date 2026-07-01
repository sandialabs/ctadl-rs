/* GAP: designated initializer `{.a = src}`. The tainted value lands in field `a`, read
   back at the sink. CTADL's `initializer_list` handling (positional `{x,0}`) does not
   cover the designated form on this branch (expected ERR 78). DFSan: flow.
   Expected harness verdict: frontend-error. */
int source();
void sink(int);
struct S { int a; int b; };

int main() {
    int x = source();
    struct S s = { .a = x, .b = 0 };   /* designated initializer */
    sink(s.a);
    return 0;
}
