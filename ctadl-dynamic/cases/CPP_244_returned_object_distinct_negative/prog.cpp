/* C++ returned objects / function-pointer members (spec 041), NEGATIVE (`q3_returned_object_distinct_negative`). `copy()` returns a fresh `P` whose `y` is 0. The struct is wider than 16 bytes, so it is returned in memory: DFSan unions the labels of a small struct returned in registers. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct P { int x; int y; int a; int b; int c; P(int v) : x(v), y(0), a(0), b(0), c(0) {} P copy() const { return P(x); } int gety() const { return y; } };
int main() { P p(source()); sink(p.copy().gety()); return 0; }
