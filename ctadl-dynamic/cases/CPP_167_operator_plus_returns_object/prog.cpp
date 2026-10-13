/* C++ member operators (spec 031), POSITIVE (`o9_op_plus_member_value`). `a + b` returns a new object built in the body. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct V { int v; V() : v(0) {} V operator+(const V& o) { V r; r.v = v + o.v; return r; } };
int main() { V a; V b; a.v = source(); V c = a + b; sink(c.v); return 0; }
