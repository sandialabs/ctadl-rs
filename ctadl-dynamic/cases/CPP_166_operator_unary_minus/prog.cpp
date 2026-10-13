/* C++ member operators (spec 031), POSITIVE (`o_a6_op_unary_minus_sinks`). `-a` is `a.operator-()`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; S() : v(0) {} int operator-() { sink(v); return 0; } };
int main() { S a; a.v = source(); int r = -a; return r; }
