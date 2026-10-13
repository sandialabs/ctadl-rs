/* C++ implicit member calls (spec 035), NEGATIVE (`i7_distinct_negative`). `f()` calls `getw()`, which reads only the clean `w`. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct S { int v; int w; int getw() { return w; } int f() { return getw(); } };
int main() { S s; s.v = source(); s.w = 0; sink(s.f()); return 0; }
