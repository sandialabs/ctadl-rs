/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `(*p).set(source()); sink(b.v)` with `B* p = &b`. `(*p).m()` dispatches on `*p` as `p->m()` does. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
int main() { B b; B* p = &b; (*p).set(source()); sink(b.v); return 0; }
