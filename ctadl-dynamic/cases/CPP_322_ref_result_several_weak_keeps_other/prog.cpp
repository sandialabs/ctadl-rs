/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& choose(int& a, int& b, int c) { if (c) return a; return b; }`; `y = source(); choose(x, y, 1) = 0; sink(y)`: the write is weak, so `y` keeps its taint. A strong write would lose it. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
int main() { int x = 0; int y = source(); choose(x, y, 1) = 0; sink(y); return 0; }
