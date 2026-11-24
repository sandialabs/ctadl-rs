/* C++ reference parameters (F8 template arm), POSITIVE: `template <class T> void setT(T& o, T v) { o = v; }`; `setT(x, source()); sink(x)`. Resolved by spec 052: a template `T&` is a dual parameter, whose write is mirrored into its cell, and the call passes `x` as a dual cell. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

template <class T> void setT(T& o, T v) { o = v; }
int main() { int x = 0; setT(x, source()); sink(x); return 0; }
