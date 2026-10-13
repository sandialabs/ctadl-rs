/* C++ implicit member calls (spec 035), NEGATIVE (`i10_overload_arity_negative`). `g(1)` must reach only the arity-1 overload (returns 0), not the niladic one (returns v). s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; int g(int x) { return 0; } int g() { return v; } int f() { return g(1); } };
int main() { S s; s.v = source(); sink(s.f()); return 0; }
