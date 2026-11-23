/* C++ pointers follow the `*p` model (spec 050), NEGATIVE: `B a; B b; B* p = &a; p->set(source()); sink(b.v)`. `*p` is `a`, never `b`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int main() { B a; B b; B* p = &a; p->set(source()); sink(b.v); return 0; }
