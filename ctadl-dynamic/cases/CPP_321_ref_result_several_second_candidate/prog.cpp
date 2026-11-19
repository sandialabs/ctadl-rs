/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& choose(int& a, int& b, int c) { if (c) return a; return b; }`; `choose(x, y, 0) = source(); sink(y)`: both candidates are written weakly. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = 0; choose(x, y, 0) = source(); sink(y); return 0; }
