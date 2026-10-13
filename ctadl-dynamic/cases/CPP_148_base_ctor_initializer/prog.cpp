/* C++ subobject construction (spec 029), POSITIVE (`s3_base_ctor_explicit`): `D(int x) : Base(x)`
   constructs the base subobject through `Base::Base(this, x)`. The base entry of the initializer
   list used to be skipped (it names a type, not a member). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; Base(int x) : v(x) {} };
struct D : Base { D(int x) : Base(x) {} };
int main() { D d(source()); sink(d.v); return 0; }
