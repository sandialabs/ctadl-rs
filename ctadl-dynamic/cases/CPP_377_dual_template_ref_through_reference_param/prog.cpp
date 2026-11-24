/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `void f(int& x) { setT(x, source()); }` with a template `setT(T& o, T v)`. The reference cell parameter is a scalar variable. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

template <class T> void setT(T& o, T v) { o = v; }
void f(int& x) { setT(x, source()); }
int main() { int x = 0; f(x); sink(x); return 0; }
