/* C++ member operators (spec 031), POSITIVE (`o_a8_op_postinc_writes_member`). `c++` is `c.operator++(0)`, the dummy int marking postfix. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct C { int v; C() : v(0) {} C operator++(int) { C old = *this; v = source(); return old; } };
int main() { C c; c++; sink(c.v); return 0; }
