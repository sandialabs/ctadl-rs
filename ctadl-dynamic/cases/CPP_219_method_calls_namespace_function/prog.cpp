/* C++ name lookup (spec 037), POSITIVE (`n7_method_calls_namespace_function`). A method of `ns::S` calling `id(x)` finds `ns::id`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

namespace ns { int id(int x) { return x; } struct S { int v; void set(int x) { v = id(x); } }; }
int main() { ns::S s; s.set(source()); sink(s.v); return 0; }
