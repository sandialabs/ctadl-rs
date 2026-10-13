/* C++ structured bindings (spec 045), NEGATIVE (`b5_binding_distinct_negative`). `y` is `p.b`, which is clean; only `p.a` is tainted. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int a; int b; };
int main() { P p; p.a = source(); p.b = 0; auto [x, y] = p; sink(y); return 0; }
