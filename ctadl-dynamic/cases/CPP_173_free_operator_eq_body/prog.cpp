/* C++ free operators (spec 032), POSITIVE (`f2_free_op_eq_sinks`). The free `operator==` body sinks its left operand's member. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; };
bool operator==(const S& a, const S& b) { sink(a.v); return a.v == b.v; }
int main() { S a; S b; a.v = source(); b.v = 0; bool r = (a == b); return r ? 0 : 1; }
