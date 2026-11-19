/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& choose(int& a, int& b, int c) { if (c) return a; return b; }`; `y = source(); clear(choose(x, y, 1)); sink(y)`, where `clear` writes 0: the copy back is weak, so `y` keeps its taint. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
void clear(int& o) { o = 0; }
int main() { int x = 0; int y = source(); clear(choose(x, y, 1)); sink(y); return 0; }
