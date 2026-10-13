/* C++ subobject construction (spec 029), NEGATIVE (`s7_base_ctor_distinct_negative`): the base
   constructor taints `v`; `u` stays 0. s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; int u; Base(int x) : v(x), u(0) {} };
struct D : Base { D(int x) : Base(x) {} };
int main() { D d(source()); sink(d.u); return 0; }
