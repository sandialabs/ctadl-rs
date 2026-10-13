/* C++ subobject construction (spec 029), POSITIVE (`s2_base_default_ctor`). A derived constructor
   that does not name its base still runs the base's default constructor first; it used to be
   skipped, so the base's members were never initialized. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { int v; Base() : v(source()) {} };
struct D : Base { int w; D() : w(0) {} };
int main() { D d; sink(d.v); return 0; }
