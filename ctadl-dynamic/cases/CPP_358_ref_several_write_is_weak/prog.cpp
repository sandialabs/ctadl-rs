/* C++ reference locals bind their locations (spec 051), POSITIVE: `int x = source(); int& r = choose(x, y, 0); r = 0; sink(x)`. `r` is `y` at run time, so `x` keeps its taint: the write must be weak. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = source(); int y = 0; int& r = choose(x, y, 0); r = 0; sink(x); return 0; }
