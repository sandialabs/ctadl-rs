/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `int& choose(int& a, int& b, int c) { if (c) return a; return b; }`; `set(choose(x, y, 1)); sink(x)`: the cell is copied back weakly into both candidates. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

int& choose(int& a, int& b, int c) { if (c) return a; return b; }
void set(int& o) { o = source(); }
int main() { int x = 0; int y = 0; set(choose(x, y, 1)); sink(x); return 0; }
