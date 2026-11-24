/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `auto f = [](int& a, int b) { a = b; }; f(x, source()); sink(x)`. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main() { auto f = [](int& a, int b) { a = b; }; int x = 0; f(x, source()); sink(x); return 0; }
