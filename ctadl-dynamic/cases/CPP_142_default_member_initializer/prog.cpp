/* C++ default member initializers (spec 028), POSITIVE (`n3_nsdmi_no_ctor`): a class with no
   constructor but `int v = source();` gets the implicit default constructor C++ defines, which
   runs the initializer. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct B { int v = source(); int get() { return v; } };
int main() { B b; sink(b.get()); return 0; }
