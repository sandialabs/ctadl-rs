/* C++ implicit member calls (spec 035), POSITIVE (`i4_inherited_call`). A derived method calling an inherited method unqualified. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int get() { return v; } }; struct D : B { int twice() { return get() + get(); } };
int main() { D d; d.v = source(); sink(d.twice()); return 0; }
