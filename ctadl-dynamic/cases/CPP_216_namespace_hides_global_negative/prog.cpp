/* C++ name lookup (spec 037), NEGATIVE (`n6_namespace_hides_global_negative`). Inside `ns`, `pick` is `ns::pick` (returns 0), not the global one. This used to be a precision gap (it resolved to the global). s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

int pick(int x) { return x; }
namespace ns { int pick(int x) { return 0; } int wrap(int x) { return pick(x); } }
int main() { sink(ns::wrap(source())); return 0; }
