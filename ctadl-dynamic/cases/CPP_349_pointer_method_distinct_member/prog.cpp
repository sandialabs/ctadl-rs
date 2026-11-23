/* C++ pointers follow the `*p` model (spec 050), NEGATIVE: `b.w = 0; B* p = &b; p->set(source()); sink(b.w)`. `set` writes `v`, never `w`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; int w; void set(int x) { v = x; } };
int main() { B b; b.w = 0; B* p = &b; p->set(source()); sink(b.w); return 0; }
