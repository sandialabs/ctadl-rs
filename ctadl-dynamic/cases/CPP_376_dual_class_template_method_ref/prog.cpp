/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `template <class T> struct Acc { void put(T& o, T v) { o = v; } }`; `Acc<int> a; a.put(x, source()); sink(x)`. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

template <class T> struct Acc { void put(T& o, T v) { o = v; } };
int main() { Acc<int> a; int x = 0; a.put(x, source()); sink(x); return 0; }
