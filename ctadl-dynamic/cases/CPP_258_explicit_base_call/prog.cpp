/* C++ qualified calls and constructor forms (spec 044), POSITIVE (`s01_explicit_base_call`). `Base::set(x)` in an override used to be a direct call with no receiver; it is a non-virtual call on `this`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; virtual void set(int x) { v = x; } virtual ~Base() {} };
struct D : Base { int w; void set(int x) override { Base::set(x); w = 0; } };
int main() { D d; d.set(source()); sink(d.v); return 0; }
