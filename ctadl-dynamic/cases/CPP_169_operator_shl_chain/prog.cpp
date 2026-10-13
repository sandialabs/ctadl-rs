/* C++ member operators (spec 031), POSITIVE (`o16_op_shl_chain`). `s << 1 << x` chains on the receiver `operator<<` returns (`return *this`). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; int w; S() : v(0), w(0) {} S& operator<<(int x) { v = v + x; return *this; } };
int main() { S s; s << 1 << source(); sink(s.v); return 0; }
