/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `B b; B* p = &b; p->set(source()); sink(b.v)`. `p->set` dispatches on `*p`, b's storage `b.deref` (`&b` makes b a cell). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int main() { B b; B* p = &b; p->set(source()); sink(b.v); return 0; }
