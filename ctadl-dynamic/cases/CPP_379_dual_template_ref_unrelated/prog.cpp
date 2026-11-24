/* C++ reference arguments through indirect calls and template `T&` (spec 052), NEGATIVE: `setT(x, source()); sink(y)` with a template `setT(T& o, T v)`. s=none d=none. */
extern "C" int source();
extern "C" void sink(int);

template <class T> void setT(T& o, T v) { o = v; }
int main() { int x = 0; int y = 0; setT(x, source()); sink(y); return 0; }
