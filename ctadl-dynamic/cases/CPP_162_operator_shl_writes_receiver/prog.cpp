/* C++ member operators (spec 031), POSITIVE (`o_a1_op_shl_writes_receiver`). A binary operator on an object used to lower as the built-in operator, blending operands, and never ran the member body. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; S() : v(0) {} S& operator<<(int x) { v = x; return *this; } };
int main() { S s; s << source(); sink(s.v); return 0; }
