/* C++ templates (spec 033), POSITIVE (`t4_class_template_ctor`). `Box<int> b(x)` constructs through `Box::Box`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class T> struct Box { T v; Box(T x) : v(x) {} T get() { return v; } };
int main() { Box<int> b(source()); sink(b.get()); return 0; }
