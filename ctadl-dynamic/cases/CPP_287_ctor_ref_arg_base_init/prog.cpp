/* C++ constructor reference arguments (spec 047), POSITIVE: `D(int& o) : B(o)` passes its own reference cell on to the base constructor, which writes it; two copies back reach `x`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int k; B(int& o) : k(0) { o = source(); } };
struct D : B { D(int& o) : B(o) {} };
int main() { int x = 0; D d(x); sink(x); return 0; }
