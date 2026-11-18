/* C++ constructor reference arguments (spec 047), POSITIVE: `B b(pick(x), 0)` passes the `int&` result of a call to the constructor's `int& o`. The cell is copied back into `x`, the place `pick` returns (spec 048; was known gap F8). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o, int n) : k(n) { o = source(); } };
int& pick(int& v) { return v; }
int main() { int x = 0; B b(pick(x), 0); sink(x); return 0; }
