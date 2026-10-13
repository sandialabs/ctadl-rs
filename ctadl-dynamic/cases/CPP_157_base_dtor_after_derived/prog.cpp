/* C++ destruction chain (spec 030), POSITIVE (`d2_base_dtor_after_derived`): destroying a `D`
   runs `~D` and then the base's `~Base`. It used to stop at the most-derived destructor.
   s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; Base() : v(0) {} ~Base() { sink(v); } };
struct D : Base { int w; D() : w(0) {} ~D() { w = 1; } };
int main() { { D d; d.v = source(); } return 0; }
