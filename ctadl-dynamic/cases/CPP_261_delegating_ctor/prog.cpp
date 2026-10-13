/* C++ qualified calls and constructor forms (spec 044), POSITIVE (`s11_delegating_ctor`). `S(int x) : S(x, 0)` runs the target constructor on this. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; int w; S(int x, int y) : v(x), w(y) {} S(int x) : S(x, 0) {} };
int main() { S s(source()); sink(s.v); return 0; }
