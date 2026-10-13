/* C++ member operators (spec 031), POSITIVE (`o_a2_op_eq_sinks_in_body`). The body of `operator==` sinks the receiver's member; it only runs if the call is emitted. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; S() : v(0) {} bool operator==(const S& o) { sink(v); return true; } };
int main() { S a; S b; a.v = source(); bool r = (a == b); return r ? 0 : 1; }
