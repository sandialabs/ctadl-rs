/* C++ reference arguments through indirect calls and template `T&` (spec 052), POSITIVE: `[](int& o) { o += source(); }` called in a loop. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int main(int argc, char**) { auto f = [](int& o) { o += source(); }; int x = 0; for (int i = 0; i < argc; i++) f(x); sink(x); return 0; }
