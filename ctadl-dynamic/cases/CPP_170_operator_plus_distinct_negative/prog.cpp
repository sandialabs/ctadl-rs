/* C++ member operators (spec 031), NEGATIVE (`o_a7_op_plus_distinct`). `a + 1` runs a body that reads only the untainted `w`. It used to be a precision gap (operands blended); the dispatch makes it exact. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; int w; S() : v(0), w(0) {} int operator+(int x) { return w + x; } };
int main() { S a; a.v = source(); int r = a + 1; sink(r); return 0; }
