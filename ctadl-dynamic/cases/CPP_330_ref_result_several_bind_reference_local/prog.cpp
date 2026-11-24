/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& choose(int& a, int& b, int c) { if (c) return a; return b; }`; `int& r = choose(x, y, 1); r = source(); sink(x)`. F9's binding arm, resolved by spec 051: a reference bound to several locations aliases a places proxy, written weakly on each. s=flow d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = 0; int& r = choose(x, y, 1); r = source(); sink(x); return 0; }
