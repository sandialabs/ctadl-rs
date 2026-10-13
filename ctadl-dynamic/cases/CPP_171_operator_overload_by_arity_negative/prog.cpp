/* C++ member operators (spec 031), NEGATIVE (`o15_op_minus_binary_negative`). unary `operator-()` returns the tainted `v`; binary `operator-(int)` returns its argument. `a - 1` must reach only the binary overload. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct V { int v; V() : v(0) {} int operator-() { return v; } int operator-(int x) { return x; } };
int main() { V a; a.v = source(); sink(a - 1); return 0; }
