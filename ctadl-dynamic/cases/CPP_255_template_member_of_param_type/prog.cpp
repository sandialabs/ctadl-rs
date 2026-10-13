/* C++ template-dependent calls (spec 043), POSITIVE (`d3_template_member_of_param_type`). `inner.get()` with `T inner` a member of a class template. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

struct Item { int v; int get() { return v; } };
template <class T> struct Wrap { T inner; int read() { return inner.get(); } };
int main() { Wrap<Item> w; w.inner.v = source(); sink(w.read()); return 0; }
