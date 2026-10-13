/* C++ structured bindings (spec 045), POSITIVE (`b7_binding_from_call`). Binding an object a function returns by value. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int a; int b; };
P make(int v) { P p; p.a = 0; p.b = v; return p; }
int main() { auto [x, y] = make(source()); sink(y); return 0; }
