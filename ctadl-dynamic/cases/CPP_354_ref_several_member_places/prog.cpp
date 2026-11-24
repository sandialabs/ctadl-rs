/* C++ reference locals bind their locations (spec 051), POSITIVE: `int& r = argc > 0 ? a.v : b.v; r = source(); sink(a.v)`. Member candidates. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

struct P { int v; int w; };
int main(int argc, char**) { P a{0, 0}; P b{0, 0}; int& r = argc > 0 ? a.v : b.v; r = source(); sink(a.v); return 0; }
