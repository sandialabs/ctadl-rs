/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `[&y](int& o) { o = source(); y = 1; }` called `f(x); sink(x + y)`. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main() { int y = 0; auto f = [&y](int& o) { o = source(); y = 1; }; int x = 0; f(x); sink(x + y); return 0; }
