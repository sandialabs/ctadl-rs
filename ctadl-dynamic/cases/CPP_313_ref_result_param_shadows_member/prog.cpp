/* C++ reference results as locations (spec 048), POSITIVE: `int& B::f(int& v) { return v; }` with a member `v` too: the result is the parameter, so `b.f(x) = source()` stores into `x`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int& f(int& v) { return v; } };
int main() { B b; b.v = 0; int x = 0; b.f(x) = source(); sink(x); return 0; }
