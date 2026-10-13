/* C++ default construction (spec 028), POSITIVE (`n14_overloaded_default_ctor`): with `B()` and
   `B(int)`, `B b;` resolves to the niladic overload (`B::B#0`). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v; B() : v(source()) {} B(int x) : v(x) {} };
int main() { B b; sink(b.v); return 0; }
