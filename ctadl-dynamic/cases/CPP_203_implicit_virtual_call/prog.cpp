/* C++ implicit member calls (spec 035), POSITIVE (`i3_virtual_call`). A virtual method called unqualified in a base method dispatches by CHA to the override. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Base { virtual int get() { return 0; } int twice() { return get() + get(); } virtual ~Base() {} };
struct D : Base { int v; int get() { return v; } };
int main() { D d; d.v = source(); sink(d.twice()); return 0; }
