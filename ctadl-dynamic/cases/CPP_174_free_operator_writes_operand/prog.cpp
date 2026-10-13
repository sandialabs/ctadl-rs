/* C++ free operators (spec 032), POSITIVE (`f3_free_op_shl_writes`). `operator<<(S& s, int x)` writes through its reference parameter. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; };
S& operator<<(S& s, int x) { s.v = x; return s; }
int main() { S s; s.v = 0; s << source(); sink(s.v); return 0; }
