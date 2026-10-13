/* C++ name lookup (spec 037), POSITIVE (`n8_using_namespace_class`). `using namespace ns; B b;` declares an `ns::B`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

namespace ns { struct B { int v; int get() { return v; } }; }
using namespace ns;
int main() { B b; b.v = source(); sink(b.get()); return 0; }
