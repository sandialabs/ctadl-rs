/* C++ returned objects / function-pointer members (spec 041), POSITIVE (`q4_namespaced_factory_chain`). `ns::make(x).get()`: the return type `P` is resolved in the callee's namespace. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

namespace ns { struct P { int x; int get() { return x; } }; P make(int v) { P p; p.x = v; return p; } }
int main() { sink(ns::make(source()).get()); return 0; }
