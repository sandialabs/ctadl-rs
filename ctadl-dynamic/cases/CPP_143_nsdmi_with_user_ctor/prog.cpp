/* C++ default member initializers (spec 028), POSITIVE (`n4_nsdmi_with_ctor`): a user
   constructor that leaves `v` out of its initializer list applies `v`'s default initializer.
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v = source(); int w; B(int x) : w(x) {} };
int main() { B b(0); sink(b.v); return 0; }
