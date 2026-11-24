/* C++ reference locals bind their locations (spec 051), POSITIVE: `int& r = choose(x, y, 1); x = source(); sink(r)`. A read through the reference blends the candidates' current values. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = 0; int& r = choose(x, y, 1); x = source(); sink(r); return 0; }
