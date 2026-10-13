/* C++ implicit member calls (spec 035), POSITIVE (`i5_ctor_calls_method`). A constructor calling a setter unqualified. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; void set(int x) { v = x; } S(int x) { set(x); } };
int main() { S s(source()); sink(s.v); return 0; }
