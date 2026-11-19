/* C++ reference results through pointers and with several places (spec 049), POSITIVE: `y = source(); clear(c ? x : y); sink(y)` with `c = 1`, where `clear` writes 0: the copy back is weak, so `y` keeps its taint. s=flow d=flow agree. */
extern "C" int source();
extern "C" void sink(int);

void clear(int& o) { o = 0; }
int main() { int x = 0; int y = source(); int c = 1; clear(c ? x : y); sink(y); return 0; }
