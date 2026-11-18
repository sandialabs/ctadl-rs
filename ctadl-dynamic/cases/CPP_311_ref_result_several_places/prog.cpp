/* C++ reference results as locations (spec 048), POSITIVE, KNOWN GAP F9: `int& choose(int& a, int& b, int c) { if (c) return a; return b; }`; `choose(x, y, 1) = source(); sink(x)`. Several places need a weak update. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = 0; choose(x, y, 1) = source(); sink(x); return 0; }
