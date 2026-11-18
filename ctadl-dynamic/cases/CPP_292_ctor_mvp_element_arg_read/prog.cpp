/* C++ constructor reference arguments (spec 047), POSITIVE: `B b(a[1])` (most-vexing parse) passes the element `a[1]`, not the array `a`, to a by-value `B(int in)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B(int in) : v(in) {} };
int main() { int a[2] = {0, 0}; a[1] = source(); B b(a[1]); sink(b.v); return 0; }
