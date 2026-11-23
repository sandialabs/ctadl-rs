/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `B a; B* p = &a; a.set(source()); sink(a.get())`. The receiver's write-back stores each member by name, so `a.deref.v` is an admissible path. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int main() { B a; B* p = &a; a.set(source()); sink(a.get() + (p != 0)); return 0; }
