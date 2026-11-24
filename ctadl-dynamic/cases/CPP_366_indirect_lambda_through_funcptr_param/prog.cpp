/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `void apply(void (*f)(int&), int& x) { f(x); }`; `apply([](int& o) { o = source(); }, x); sink(x)`. The lambda's `int&` is a reference cell. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

void apply(void (*f)(int&), int& x) { f(x); }
int main() { int x = 0; apply([](int& o) { o = source(); }, x); sink(x); return 0; }
