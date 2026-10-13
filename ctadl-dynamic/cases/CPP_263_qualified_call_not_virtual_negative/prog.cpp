/* C++ qualified calls and constructor forms (spec 044), NEGATIVE (`x1_qualified_call_not_virtual_negative`). `Base::set(x)` must call exactly `Base::set` (which stores 0), not the override (which stores x). s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; Base() : v(0) {} virtual void set(int x) { v = 0; } virtual ~Base() {} };
struct D : Base { void set(int x) override { v = x; } void f(int x) { Base::set(x); } };
int main() { D d; d.f(source()); sink(d.v); return 0; }
