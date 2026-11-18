/* C++ reference results as locations (spec 048), POSITIVE: `a->pass(x) = source()`, where both CHA targets `A::pass` and `D::pass` return `o`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct A { virtual int& pass(int& o) { return o; } virtual ~A() {} };
struct D : A { int& pass(int& o) override { return o; } };
int main() { D d; A* a = &d; int x = 0; a->pass(x) = source(); sink(x); return 0; }
