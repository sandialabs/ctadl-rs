/* C++ templates (spec 033), POSITIVE (`t12_using_alias_template`). `using IntBox = Box<int>;` names `Box`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class T> struct Box { T v; void set(T x) { v = x; } T get() { return v; } };
using IntBox = Box<int>;
int main() { IntBox b; b.set(source()); sink(b.get()); return 0; }
