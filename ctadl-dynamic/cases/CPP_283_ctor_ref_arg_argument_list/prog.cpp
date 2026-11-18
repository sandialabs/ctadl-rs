/* C++ constructor reference arguments (spec 047), POSITIVE: `B b(x, 0)` (an `argument_list`) with `B(int& o, int n)` writing `o = source()`; the cell is copied back into `x`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o, int n) : k(n) { o = source(); } };
int main() { int x = 0; B b(x, 0); sink(x); return 0; }
