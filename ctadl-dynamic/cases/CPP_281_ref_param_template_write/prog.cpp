/* C++ reference parameters after #154, POSITIVE, KNOWN GAP F8: a template parameter's reference `T& o` keeps the root model (its method calls dispatch on the parameter itself, spec 043), so `o = v` is a rebinding that does not reach the caller. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

template <class T> void setT(T& o, T v) { o = v; }
int main() { int x = 0; setT(x, source()); sink(x); return 0; }
