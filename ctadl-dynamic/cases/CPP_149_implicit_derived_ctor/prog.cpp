/* C++ subobject construction (spec 029), POSITIVE (`s4_implicit_derived_ctor`): a derived class
   with no constructor gets the implicit one, which runs the base's default constructor.
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; Base() : v(source()) {} };
struct D : Base { int w; };
int main() { D d; sink(d.v); return 0; }
