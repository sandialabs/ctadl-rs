/* C++ name lookup (spec 037), POSITIVE (`n1_using_namespace`). `using namespace ns; id(x)` calls `ns::id`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

namespace ns { int id(int x) { return x; } }
using namespace ns;
int main() { sink(id(source())); return 0; }
