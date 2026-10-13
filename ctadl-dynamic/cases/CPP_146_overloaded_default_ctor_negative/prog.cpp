/* C++ default construction (spec 028), NEGATIVE (`n15_overloaded_default_ctor_negative`): the
   niladic constructor sets `v(0)`; only `B(int)` would carry taint, and it is not called.
   s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B() : v(0) {} B(int x) : v(x) {} };
int main() { int t = source(); B b; sink(b.v); return t - t; }
