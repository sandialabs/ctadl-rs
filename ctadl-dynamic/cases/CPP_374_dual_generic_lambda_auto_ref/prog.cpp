/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `auto f = [](auto& o) { o = source(); }; f(x); sink(x)`. An `auto&` is a dual parameter. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main() { auto f = [](auto& o) { o = source(); }; int x = 0; f(x); sink(x); return 0; }
