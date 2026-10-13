/* C++ implicit member calls (spec 035), POSITIVE (`i2_static_call`). An unqualified static member function is `S::id(x)`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);
struct S { int v; static int id(int x) { return x; } void set(int x) { v = id(x); } };
int main() { S s; s.set(source()); sink(s.v); return 0; }
