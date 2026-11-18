/* C++ constructor reference arguments (spec 047), POSITIVE, KNOWN GAP F8: `B b(pick(x), 0)` passes the `int&` result of a call to the constructor's `int& o`. The cell is not copied back, because a call is no plain lvalue at any call site (`set(pick(x))` too). s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o, int n) : k(n) { o = source(); } };
int& pick(int& v) { return v; }
int main() { int x = 0; B b(pick(x), 0); sink(x); return 0; }
