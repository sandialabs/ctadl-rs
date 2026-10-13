/* C++ templates (spec 033), POSITIVE (`t2_class_template`). `Box<int> b` used to be no object at all (its type is a `template_type`), so `b.set(x)` dispatched nowhere. A template is one class for all instances. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class T> struct Box { T v; void set(T x) { v = x; } T get() { return v; } };
int main() { Box<int> b; b.set(source()); sink(b.get()); return 0; }
