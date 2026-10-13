/* C++ structured bindings (spec 045), POSITIVE (`b3_ref_binding_write`). `auto& [x, y] = p; y = source();` writes `p.b`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int a; int b; };
int main() { P p; p.a = 0; p.b = 0; auto& [x, y] = p; y = source(); sink(p.b); return 0; }
