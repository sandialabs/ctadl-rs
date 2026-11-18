/* C++ constructor reference arguments (spec 047), NEGATIVE: `B b(x)` taints `x` through the constructor's `int& o`, and `x = 0` then overwrites it before `sink(x)`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } };
int main() { int x = 0; B b(x); x = 0; sink(x); return 0; }
