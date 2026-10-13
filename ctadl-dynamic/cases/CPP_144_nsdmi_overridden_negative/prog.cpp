/* C++ default member initializers (spec 028), NEGATIVE (`n5_nsdmi_overridden_negative`): the
   constructor's own `v(0)` replaces the default `v = source()`, so the default never runs.
   The precision guard: it fails (s=flow) if a constructor applies a default its list overrides.
   s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v = source(); B() : v(0) {} };
int main() { B b; sink(b.v); return 0; }
