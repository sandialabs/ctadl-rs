/* C++ structured bindings (spec 045), POSITIVE (`b1_value_binding`). `auto [x, y] = p;` used to be a frontend error; each name copies the member at its position. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int a; int b; };
int main() { P p; p.a = source(); p.b = 0; auto [x, y] = p; sink(x); return 0; }
