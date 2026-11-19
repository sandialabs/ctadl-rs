/* C++ reference results as locations (spec 048), POSITIVE: `int& choose(int& a, int& b, int c) { if (c) return a; return b; }`; `choose(x, y, 1) = source(); sink(x)`: both candidates are written weakly (spec 049; was known gap F9). s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = 0; choose(x, y, 1) = source(); sink(x); return 0; }
