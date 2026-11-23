/* C++ pointers follow the `*p` model (spec 050), POSITIVE: `~B() { sink(v); }`; `B* p = new B(); p->v = source(); delete p`. `delete p` destroys `*p`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct B { int v; B() : v(0) {} ~B() { sink(v); } };
int main() { B* p = new B(); p->v = source(); delete p; return 0; }
