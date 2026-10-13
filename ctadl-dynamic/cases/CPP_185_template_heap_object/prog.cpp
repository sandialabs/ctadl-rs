/* C++ templates (spec 033), POSITIVE (`t8_template_ptr_member`). `new Box<int>()` and `p->set(x)` on a template instance -- a frontend error before. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class T> struct Box { T v; void set(T x) { v = x; } T get() { return v; } };
int main() { Box<int>* p = new Box<int>(); p->set(source()); sink(p->get()); delete p; return 0; }
