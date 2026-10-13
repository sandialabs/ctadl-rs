/* C++ name lookup (spec 037), POSITIVE (`n10_class_in_own_namespace`). Inside `ns`, `B b;` declares an `ns::B`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

namespace ns { struct B { int v; int get() { return v; } }; int use(int x) { B b; b.v = x; return b.get(); } }
int main() { sink(ns::use(source())); return 0; }
