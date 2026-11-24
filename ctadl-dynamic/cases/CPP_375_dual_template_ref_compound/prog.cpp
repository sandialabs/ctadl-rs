/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `template <class T> void addT(T& o, T v) { o += v; }`; `addT(x, source()); sink(x)`. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

template <class T> void addT(T& o, T v) { o += v; }
int main() { int x = 0; addT(x, source()); sink(x); return 0; }
