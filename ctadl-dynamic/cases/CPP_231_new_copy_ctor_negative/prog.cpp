/* C++ new copy construction (spec 039), NEGATIVE (`h2_new_copy_distinct_negative`). The copy constructor sets `v(0)`; only it runs. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; int w; B(int x) : v(x), w(0) {} B(const B& o) : v(0), w(0) {} };
int main() { B a(source()); B* p = new B(a); sink(p->v); delete p; return 0; }
