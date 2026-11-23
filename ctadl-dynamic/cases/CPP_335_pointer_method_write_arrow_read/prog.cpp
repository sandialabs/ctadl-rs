/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `p->set(source()); sink(p->v)` with `B* p = &b`. The method's write and the `->` read both name `b.deref.v`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int main() { B b; B* p = &b; p->set(source()); sink(p->v); return 0; }
