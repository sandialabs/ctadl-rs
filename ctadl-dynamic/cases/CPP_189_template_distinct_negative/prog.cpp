/* C++ templates (spec 033), NEGATIVE (`t7_template_distinct_negative`). `set` taints `v`; `getw()` reads `w`. One body for all instances must still keep members apart. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

template <class T> struct Box { T v; T w; Box() : v(0), w(0) {} void set(T x) { v = x; } T getw() { return w; } };
int main() { Box<int> b; b.set(source()); sink(b.getw()); return 0; }
