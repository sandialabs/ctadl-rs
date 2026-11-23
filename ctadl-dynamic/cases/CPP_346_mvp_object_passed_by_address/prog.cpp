/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `take(B* p) { sink(p->v); }`; `B a(source()); take(&a)`. `B a(x)` (the most vexing parse) declares an object, so `&a` makes `a` a cell constructed at `a.deref`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} B(int x) : v(x) {} void set(int x) { v = x; } int get() { return v; } };
void take(B* p) { sink(p->v); }
int main() { B a(source()); take(&a); return 0; }
