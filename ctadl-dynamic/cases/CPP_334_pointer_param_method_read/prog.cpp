/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `int read(B* p) { return p->get(); }`; `a.set(source()); sink(read(&a))`. The callee's receiver is `*p`, `@p0.deref`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int read(B* p) { return p->get(); }
int main() { B a; a.set(source()); sink(read(&a)); return 0; }
