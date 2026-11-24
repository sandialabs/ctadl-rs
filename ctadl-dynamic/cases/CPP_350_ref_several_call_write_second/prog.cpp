/* C++ reference locals bind their locations (spec 051), POSITIVE: `int& r = choose(x, y, 0); r = source(); sink(y)`, where `choose` returns either reference parameter. The binding aliases a places proxy, and the write is weak on each candidate. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = 0; int& r = choose(x, y, 0); r = source(); sink(y); return 0; }
