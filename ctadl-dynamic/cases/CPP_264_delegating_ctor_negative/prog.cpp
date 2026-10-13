/* C++ qualified calls and constructor forms (spec 044), NEGATIVE (`x2_delegating_ctor_negative`). `S(int x) : S(0, x)` puts x in `w`; `v` stays 0. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; int w; S(int x, int y) : v(x), w(y) {} S(int x) : S(0, x) {} };
int main() { S s(source()); sink(s.v); return 0; }
