/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `B* p = new B(); p->set(source()); sink(p->v)`. `new` yields an address `O` whose object is `O.deref`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int main() { B* p = new B(); p->set(source()); sink(p->v); delete p; return 0; }
