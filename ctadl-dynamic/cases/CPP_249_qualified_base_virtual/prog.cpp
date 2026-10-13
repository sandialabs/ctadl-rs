/* C++ pointer members / new / namespaced bases (spec 042), POSITIVE (`k3_qualified_base_virtual`). `struct D : public ns::Base`: a qualified base is recorded and resolved, so the virtual call through `ns::Base&` reaches `D::get`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

namespace ns { struct Base { virtual int get() { return 0; } virtual ~Base() {} }; }
struct D : public ns::Base { int v; int get() override { return v; } };
int call(ns::Base& b) { return b.get(); }
int main() { D d; d.v = source(); sink(call(d)); return 0; }
