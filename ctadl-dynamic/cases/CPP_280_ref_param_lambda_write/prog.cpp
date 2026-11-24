/* C++ reference parameters (F8 lambda arm), POSITIVE: `auto f = [](int& o) { o = source(); }; int x = 0; f(x); sink(x)`. Resolved by spec 052: a lambda's `int&` is a reference cell, and the closure call passes `x` as a dual cell. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main() { auto f = [](int& o) { o = source(); }; int x = 0; f(x); sink(x); return 0; }
