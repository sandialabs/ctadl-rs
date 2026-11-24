/* C++ reference locals bind their locations (spec 051), POSITIVE: `void f(int& x, int& y) { int& r = choose(x, y, 1); r = source(); }`, then `f(x, y); sink(x)`. The candidates are reference cells, copied back at the caller. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
void f(int& x, int& y) { int& r = choose(x, y, 1); r = source(); }
int main() { int x = 0; int y = 0; f(x, y); sink(x); return 0; }
