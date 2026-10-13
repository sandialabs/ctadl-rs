/* C++ new copy construction (spec 039), POSITIVE (`h1_new_copy`). `new B(a)` used to resolve by arity (to `B(int)`, given an object); it runs the copy constructor. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B(int x) : v(x) {} B(const B& o) : v(o.v) {} };
int main() { B a(source()); B* p = new B(a); sink(p->v); delete p; return 0; }
