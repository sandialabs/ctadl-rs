/* C++ templates (spec 033), POSITIVE (`t5_class_template_out_of_line`). Out-of-line members are written `Box<T>::set`; the scope erases to `Box`. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

template <class T> struct Box { T v; void set(T x); T get(); };
template <class T> void Box<T>::set(T x) { v = x; }
template <class T> T Box<T>::get() { return v; }
int main() { Box<int> b; b.set(source()); sink(b.get()); return 0; }
