/* C++ qualified calls and constructor forms (spec 044), POSITIVE (`s12_inherited_ctor_using`). `using Base::Base;` lets `D d(x)` construct through `Base(int)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; Base(int x) : v(x) {} };
struct D : Base { using Base::Base; };
int main() { D d(source()); sink(d.v); return 0; }
