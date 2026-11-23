/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `p->v = source(); sink(p->get())` with `B* p = &b`. The `->` write and the method's read both name `b.deref.v`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int main() { B b; B* p = &b; p->v = source(); sink(p->get()); return 0; }
