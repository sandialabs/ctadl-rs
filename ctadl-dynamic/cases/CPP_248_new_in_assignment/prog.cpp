/* C++ pointer members / new / namespaced bases (spec 042), POSITIVE (`k1_new_in_assignment`). `p = new B(x)` as an assignment. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B(int x) : v(x) {} int get() { return v; } };
int main() { B* p; p = new B(source()); sink(p->get()); delete p; return 0; }
