/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `template <class F> void apply(F f, int& x) { f(x); }` called with a lambda writing its `int&`. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

template <class F> void apply(F f, int& x) { f(x); }
int main() { int x = 0; apply([](int& o) { o = source(); }, x); sink(x); return 0; }
