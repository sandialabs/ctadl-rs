/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `int g`; `setT(g, source()); sink(g)` with a template `setT(T& o, T v)`. A scalar global is passed as a dual cell. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int g = 0;
template <class T> void setT(T& o, T v) { o = v; }
int main() { setT(g, source()); sink(g); return 0; }
