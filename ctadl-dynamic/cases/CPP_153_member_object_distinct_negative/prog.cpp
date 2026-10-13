/* C++ subobject construction (spec 029), NEGATIVE (`s13_member_object_negative`): the member
   object's constructor taints `in.v`; `in.u` stays 0. The copy-out store must not blend the two.
   s=none d=none agree. */
extern "C" int source();
extern "C" void sink(int);

struct In { int v; int u; In(int x) : v(x), u(0) {} };
struct Out { In in; Out(int x) : in(x) {} };
int main() { Out o(source()); sink(o.in.u); return 0; }
