/* C++ name lookup (spec 037), POSITIVE (`n3_call_within_namespace`). `id(x)` inside `ns::wrap` used to call a nonexistent global `id`; lookup finds `ns::id`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

namespace ns { int id(int x) { return x; } int wrap(int x) { return id(x); } }
int main() { sink(ns::wrap(source())); return 0; }
