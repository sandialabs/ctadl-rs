/* C++ reference results through pointers and with several places (spec 049), POSITIVE, KNOWN GAP F9: `int& choose(int& a, int& b, int c) { if (c) return a; return b; }`; `int& r = choose(x, y, 1); r = source(); sink(x)`. A reference local cannot alias several locations, so it binds the result temp. s=none d=flow. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = 0; int& r = choose(x, y, 1); r = source(); sink(x); return 0; }
