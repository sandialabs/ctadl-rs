/* C++ member operators (spec 031), POSITIVE (`o_a3_op_preinc_writes_member`). `++c` is `c.operator++()`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct C { int v; C() : v(0) {} C& operator++() { v = source(); return *this; } };
int main() { C c; ++c; sink(c.v); return 0; }
